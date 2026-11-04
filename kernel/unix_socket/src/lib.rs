#![no_std]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use core::fmt::{self, Debug, Formatter};

use kernel_abi::{
    EADDRINUSE, EAGAIN, ECONNREFUSED, EDESTADDRREQ, EINVAL, EISCONN, ENOTCONN, EOPNOTSUPP, EPIPE,
    Errno, ShutdownHow,
};
use kernel_vfs::path::AbsoluteOwnedPath;
use spin::Mutex;

pub const SOCKET_BUFFER_BYTES: usize = 64 * 1024;

/// Wakes every task that waits on the owner. Spurious wakes are allowed.
pub trait Notify: Default + Send + Sync + 'static {
    fn notify_all(&self);
}

pub struct Registry<N: Notify> {
    bound: Mutex<BTreeMap<AbsoluteOwnedPath, Weak<UnixSocket<N>>>>,
    /// Wakes when a listen queue gains room or a listener goes away.
    waiters: N,
}

impl<N: Notify> Registry<N> {
    pub const fn new(waiters: N) -> Self {
        Self {
            bound: Mutex::new(BTreeMap::new()),
            waiters,
        }
    }

    pub fn waiters(&self) -> &N {
        &self.waiters
    }
}

pub struct UnixSocket<N: Notify> {
    registry: &'static Registry<N>,
    state: Mutex<State<N>>,
    waiters: N,
}

enum State<N: Notify> {
    Unbound,
    Bound(AbsoluteOwnedPath),
    Listening {
        path: AbsoluteOwnedPath,
        backlog: usize,
        pending: VecDeque<Arc<UnixSocket<N>>>,
    },
    Connected {
        peer: Weak<UnixSocket<N>>,
        inbox: VecDeque<u8>,
        /// No more bytes arrive. The peer shut its write side, or this side shut its read side.
        inbox_closed: bool,
        /// Writes fail. This side shut its write side, or the peer shut its read side.
        outbox_closed: bool,
    },
}

impl<N: Notify> State<N> {
    fn connected(peer: Weak<UnixSocket<N>>) -> Self {
        State::Connected {
            peer,
            inbox: VecDeque::new(),
            inbox_closed: false,
            outbox_closed: false,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            State::Unbound => "Unbound",
            State::Bound(_) => "Bound",
            State::Listening { .. } => "Listening",
            State::Connected { .. } => "Connected",
        }
    }
}

impl<N: Notify> UnixSocket<N> {
    pub fn new(registry: &'static Registry<N>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            state: Mutex::new(State::Unbound),
            waiters: N::default(),
        })
    }

    pub fn waiters(&self) -> &N {
        &self.waiters
    }

    pub fn registry(&self) -> &'static Registry<N> {
        self.registry
    }

    pub fn bind(self: &Arc<Self>, path: AbsoluteOwnedPath) -> Result<(), Errno> {
        let mut state = self.state.lock();
        if !matches!(*state, State::Unbound) {
            return Err(EINVAL);
        }
        let mut bound = self.registry.bound.lock();
        if bound.get(&path).is_some_and(|w| w.upgrade().is_some()) {
            return Err(EADDRINUSE);
        }
        bound.insert(path.clone(), Arc::downgrade(self));
        drop(bound);
        *state = State::Bound(path);
        Ok(())
    }

    pub fn listen(&self, backlog: usize) -> Result<(), Errno> {
        let mut state = self.state.lock();
        match &mut *state {
            State::Unbound => Err(EDESTADDRREQ),
            State::Connected { .. } => Err(EINVAL),
            State::Bound(path) => {
                let path = path.clone();
                *state = State::Listening {
                    path,
                    backlog,
                    pending: VecDeque::new(),
                };
                Ok(())
            }
            State::Listening { backlog: b, .. } => {
                *b = backlog;
                Ok(())
            }
        }
    }

    pub fn try_accept(&self) -> Result<Arc<Self>, Errno> {
        let mut state = self.state.lock();
        let State::Listening { pending, .. } = &mut *state else {
            return Err(EINVAL);
        };
        let peer = pending.pop_front().ok_or(EAGAIN)?;
        drop(state);
        // A connector may wait for room in the queue.
        self.registry.waiters.notify_all();
        Ok(peer)
    }

    pub fn try_connect(self: &Arc<Self>, path: &AbsoluteOwnedPath) -> Result<(), Errno> {
        let mut state = self.state.lock();
        match *state {
            State::Connected { .. } => return Err(EISCONN),
            State::Listening { .. } => return Err(EOPNOTSUPP),
            State::Bound(_) => return Err(EINVAL),
            State::Unbound => {}
        }
        let listener = self
            .registry
            .bound
            .lock()
            .get(path)
            .and_then(Weak::upgrade)
            .ok_or(ECONNREFUSED)?;
        let mut listener_state = listener.state.lock();
        let State::Listening {
            backlog, pending, ..
        } = &mut *listener_state
        else {
            return Err(ECONNREFUSED);
        };
        if pending.len() >= *backlog {
            return Err(EAGAIN);
        }
        let server = Arc::new(UnixSocket {
            registry: self.registry,
            state: Mutex::new(State::connected(Arc::downgrade(self))),
            waiters: N::default(),
        });
        *state = State::connected(Arc::downgrade(&server));
        pending.push_back(server);
        drop(listener_state);
        drop(state);
        listener.waiters.notify_all();
        Ok(())
    }

    pub fn try_read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut state = self.state.lock();
        let State::Connected {
            peer,
            inbox,
            inbox_closed,
            ..
        } = &mut *state
        else {
            return Err(ENOTCONN);
        };
        if inbox.is_empty() {
            return if *inbox_closed || peer.upgrade().is_none() {
                Ok(0)
            } else {
                Err(EAGAIN)
            };
        }
        let n = buf.len().min(inbox.len());
        for (dst, src) in buf.iter_mut().zip(inbox.drain(..n)) {
            *dst = src;
        }
        let peer = peer.clone();
        drop(state);
        // The writer may wait for room in the inbox.
        if let Some(peer) = peer.upgrade() {
            peer.waiters.notify_all();
        }
        Ok(n)
    }

    pub fn try_write(&self, buf: &[u8]) -> Result<usize, Errno> {
        let state = self.state.lock();
        let State::Connected {
            peer,
            outbox_closed,
            ..
        } = &*state
        else {
            return Err(ENOTCONN);
        };
        if *outbox_closed {
            return Err(EPIPE);
        }
        let peer = peer.clone();
        drop(state);
        let peer = peer.upgrade().ok_or(EPIPE)?;
        let mut peer_state = peer.state.lock();
        let State::Connected { inbox, .. } = &mut *peer_state else {
            unreachable!("a peer is always connected");
        };
        let free = SOCKET_BUFFER_BYTES - inbox.len();
        if free == 0 {
            return Err(EAGAIN);
        }
        let n = free.min(buf.len());
        inbox.extend(&buf[..n]);
        drop(peer_state);
        peer.waiters.notify_all();
        Ok(n)
    }

    pub fn shutdown(&self, how: ShutdownHow) -> Result<(), Errno> {
        let shut_write = matches!(how, ShutdownHow::Write | ShutdownHow::Both);
        let shut_read = matches!(how, ShutdownHow::Read | ShutdownHow::Both);
        let mut state = self.state.lock();
        let State::Connected {
            peer,
            inbox,
            inbox_closed,
            outbox_closed,
        } = &mut *state
        else {
            return Err(ENOTCONN);
        };
        if shut_write {
            *outbox_closed = true;
        }
        if shut_read {
            inbox.clear();
            *inbox_closed = true;
        }
        let peer = peer.clone();
        drop(state);
        self.waiters.notify_all();
        if let Some(peer) = peer.upgrade() {
            if let State::Connected {
                inbox_closed,
                outbox_closed,
                ..
            } = &mut *peer.state.lock()
            {
                *inbox_closed |= shut_write;
                *outbox_closed |= shut_read;
            }
            peer.waiters.notify_all();
        }
        Ok(())
    }
}

impl<N: Notify> Debug for UnixSocket<N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnixSocket")
            .field("state", &self.state.lock().name())
            .finish()
    }
}

impl<N: Notify> Drop for UnixSocket<N> {
    fn drop(&mut self) {
        match self.state.get_mut() {
            State::Bound(path) | State::Listening { path, .. } => {
                let mut bound = self.registry.bound.lock();
                // Another socket may own the name again after this one died.
                if bound.get(path).is_some_and(|w| w.strong_count() == 0) {
                    bound.remove(path);
                }
                drop(bound);
                self.registry.waiters.notify_all();
            }
            State::Connected { peer, .. } => {
                if let Some(peer) = peer.upgrade() {
                    peer.waiters.notify_all();
                }
            }
            State::Unbound => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Silent;

    impl Notify for Silent {
        fn notify_all(&self) {}
    }

    static REGISTRY: Registry<Silent> = Registry::new(Silent);

    type Socket = UnixSocket<Silent>;

    fn path(p: &str) -> AbsoluteOwnedPath {
        AbsoluteOwnedPath::try_from(p).unwrap()
    }

    fn listener(p: &str, backlog: usize) -> Arc<Socket> {
        let l = Socket::new(&REGISTRY);
        l.bind(path(p)).unwrap();
        l.listen(backlog).unwrap();
        l
    }

    /// Returns `(listener, client, server)`.
    fn pair(p: &str) -> (Arc<Socket>, Arc<Socket>, Arc<Socket>) {
        let l = listener(p, 4);
        let c = Socket::new(&REGISTRY);
        c.try_connect(&path(p)).unwrap();
        let s = l.try_accept().unwrap();
        (l, c, s)
    }

    #[test]
    fn bind_in_use_until_drop() {
        let a = Socket::new(&REGISTRY);
        a.bind(path("/a")).unwrap();
        let b = Socket::new(&REGISTRY);
        assert_eq!(b.bind(path("/a")), Err(EADDRINUSE));
        drop(a);
        assert_eq!(b.bind(path("/a")), Ok(()));
    }

    #[test]
    fn connect_refused() {
        let c = Socket::new(&REGISTRY);
        assert_eq!(c.try_connect(&path("/missing")), Err(ECONNREFUSED));
        let b = Socket::new(&REGISTRY);
        b.bind(path("/b")).unwrap();
        assert_eq!(c.try_connect(&path("/b")), Err(ECONNREFUSED));
    }

    #[test]
    fn listen_and_accept_state_errors() {
        let s = Socket::new(&REGISTRY);
        assert_eq!(s.listen(1), Err(EDESTADDRREQ));
        s.bind(path("/c")).unwrap();
        assert_eq!(s.try_accept().unwrap_err(), EINVAL);
    }

    #[test]
    fn data_both_ways_and_buffer_limit() {
        let (_l, c, s) = pair("/d");
        let mut buf = [0; 8];
        assert_eq!(c.try_write(b"abc"), Ok(3));
        assert_eq!(s.try_write(b"xy"), Ok(2));
        assert_eq!(s.try_read(&mut buf), Ok(3));
        assert_eq!(&buf[..3], b"abc");
        assert_eq!(c.try_read(&mut buf), Ok(2));
        assert_eq!(&buf[..2], b"xy");
        assert_eq!(c.try_read(&mut buf), Err(EAGAIN));

        let big = alloc::vec![7; SOCKET_BUFFER_BYTES + 1];
        assert_eq!(c.try_write(&big), Ok(SOCKET_BUFFER_BYTES));
        assert_eq!(c.try_write(b"z"), Err(EAGAIN));
        assert_eq!(s.try_read(&mut buf[..1]), Ok(1));
        assert_eq!(c.try_write(b"z"), Ok(1));
    }

    #[test]
    fn peer_drop_gives_eof_and_epipe() {
        let (_l, c, s) = pair("/e");
        c.try_write(b"q").unwrap();
        drop(c);
        let mut buf = [0; 4];
        assert_eq!(s.try_read(&mut buf), Ok(1));
        assert_eq!(s.try_read(&mut buf), Ok(0));

        let (_l2, c2, s2) = pair("/e2");
        drop(s2);
        assert_eq!(c2.try_write(b"q"), Err(EPIPE));
    }

    #[test]
    fn backlog_full() {
        let l = listener("/f", 1);
        let a = Socket::new(&REGISTRY);
        a.try_connect(&path("/f")).unwrap();
        let b = Socket::new(&REGISTRY);
        assert_eq!(b.try_connect(&path("/f")), Err(EAGAIN));
        let _s = l.try_accept().unwrap();
        assert_eq!(b.try_connect(&path("/f")), Ok(()));
    }

    #[test]
    fn listener_drop_drops_pending() {
        let l = listener("/g", 1);
        let c = Socket::new(&REGISTRY);
        c.try_connect(&path("/g")).unwrap();
        drop(l);
        assert_eq!(c.try_read(&mut [0; 4]), Ok(0));
    }

    #[test]
    fn state_errors() {
        let u = Socket::new(&REGISTRY);
        assert_eq!(u.try_read(&mut [0; 4]), Err(ENOTCONN));
        assert_eq!(u.try_write(b"x"), Err(ENOTCONN));
        let (l, c, _s) = pair("/h");
        assert_eq!(c.try_connect(&path("/h")), Err(EISCONN));
        assert_eq!(c.listen(1), Err(EINVAL));
        assert_eq!(l.try_connect(&path("/h")), Err(EOPNOTSUPP));
    }

    #[test]
    fn shutdown_write_and_read() {
        let (_l, c, s) = pair("/i");
        let mut buf = [0; 4];
        c.try_write(b"ab").unwrap();
        c.shutdown(ShutdownHow::Write).unwrap();
        assert_eq!(c.try_write(b"x"), Err(EPIPE));
        assert_eq!(s.try_read(&mut buf), Ok(2));
        assert_eq!(s.try_read(&mut buf), Ok(0));
        assert_eq!(s.try_write(b"ok"), Ok(2));

        let (_l2, c2, s2) = pair("/i2");
        c2.try_write(b"ab").unwrap();
        s2.shutdown(ShutdownHow::Read).unwrap();
        assert_eq!(s2.try_read(&mut buf), Ok(0));
        assert_eq!(c2.try_write(b"x"), Err(EPIPE));
    }
}
