#![no_std]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::fmt::{self, Debug, Formatter};

use kernel_abi::{
    EADDRINUSE, EAGAIN, ECONNREFUSED, EDESTADDRREQ, EINVAL, EISCONN, ENOBUFS, ENOTCONN, EOPNOTSUPP,
    EPIPE, Errno, ShutdownHow,
};
use kernel_vfs::path::AbsoluteOwnedPath;
use spin::Mutex;

pub const SOCKET_BUFFER_BYTES: usize = 64 * 1024;

/// A sender gets `ENOBUFS` above the limit; `EAGAIN` would block it forever
/// when the receiver never calls `recvmsg`.
pub const MAX_PENDING_ATTACHED: usize = 256;

/// Wakes every task that waits on the owner. Spurious wakes are allowed.
pub trait Notify: Default + Send + Sync + 'static {
    fn notify_all(&self);
}

/// What a take does with the items attached to its first byte.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttachedItems {
    Take,
    /// A plain read closes what it does not take, as Linux does.
    Drop,
}

struct Inbox<P> {
    bytes: VecDeque<u8>,
    written_total: u64,
    read_total: u64,
    /// Stream offset of the byte that carries the items.
    attachments: BTreeMap<u64, Vec<P>>,
}

impl<P> Inbox<P> {
    const fn new() -> Self {
        Self {
            bytes: VecDeque::new(),
            written_total: 0,
            read_total: 0,
            attachments: BTreeMap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn push(&mut self, buf: &[u8], items: Vec<P>) -> Result<usize, Errno> {
        let free = SOCKET_BUFFER_BYTES - self.bytes.len();
        if free == 0 {
            return Err(EAGAIN);
        }
        if !items.is_empty() {
            if buf.is_empty() {
                return Err(EINVAL);
            }
            let pending: usize = self.attachments.values().map(Vec::len).sum();
            if items.len() + pending > MAX_PENDING_ATTACHED {
                return Err(ENOBUFS);
            }
        }
        let byte_count = free.min(buf.len());
        self.bytes.extend(&buf[..byte_count]);
        if !items.is_empty() {
            let replaced = self.attachments.insert(self.written_total, items);
            debug_assert!(replaced.is_none(), "every push with items appends a byte");
        }
        self.written_total += byte_count as u64;
        Ok(byte_count)
    }

    /// A take never crosses into the byte that carries the next attachment.
    fn take(&mut self, buf: &mut [u8], policy: AttachedItems) -> (usize, Vec<P>) {
        let start = self.read_total;
        let items = self
            .attachments
            .remove(&start)
            .filter(|_| policy == AttachedItems::Take)
            .unwrap_or_default();
        let mut byte_count = buf.len().min(self.bytes.len());
        if let Some(next) = self.attachments.keys().next() {
            byte_count = byte_count.min(usize::try_from(next - start).unwrap_or(usize::MAX));
        }
        for (dst, src) in buf.iter_mut().zip(self.bytes.drain(..byte_count)) {
            *dst = src;
        }
        self.read_total += byte_count as u64;
        (byte_count, items)
    }

    fn discard(&mut self) {
        self.bytes.clear();
        self.attachments.clear();
        self.read_total = self.written_total;
    }
}

pub struct Registry<N: Notify, P: Send + Sync + 'static> {
    bound: Mutex<BTreeMap<AbsoluteOwnedPath, Weak<UnixSocket<N, P>>>>,
    /// Wakes when a listen queue gains room or a listener goes away.
    waiters: N,
}

impl<N: Notify, P: Send + Sync + 'static> Registry<N, P> {
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

pub struct UnixSocket<N: Notify, P: Send + Sync + 'static> {
    registry: &'static Registry<N, P>,
    state: Mutex<State<N, P>>,
    waiters: N,
}

enum State<N: Notify, P: Send + Sync + 'static> {
    Unbound,
    Bound(AbsoluteOwnedPath),
    Listening {
        path: AbsoluteOwnedPath,
        backlog: usize,
        pending: VecDeque<Arc<UnixSocket<N, P>>>,
    },
    Connected {
        peer: Weak<UnixSocket<N, P>>,
        inbox: Inbox<P>,
        /// No more bytes arrive. The peer shut its write side, or this side shut its read side.
        inbox_closed: bool,
        /// Writes fail. This side shut its write side, or the peer shut its read side.
        outbox_closed: bool,
    },
}

impl<N: Notify, P: Send + Sync + 'static> State<N, P> {
    fn connected(peer: Weak<UnixSocket<N, P>>) -> Self {
        State::Connected {
            peer,
            inbox: Inbox::new(),
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

impl<N: Notify, P: Send + Sync + 'static> UnixSocket<N, P> {
    pub fn new(registry: &'static Registry<N, P>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            state: Mutex::new(State::Unbound),
            waiters: N::default(),
        })
    }

    pub fn waiters(&self) -> &N {
        &self.waiters
    }

    pub fn registry(&self) -> &'static Registry<N, P> {
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
        self.drain(buf, AttachedItems::Drop).map(|(n, _)| n)
    }

    /// Takes the items attached to the first byte read.
    pub fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(usize, Vec<P>), Errno> {
        if buf.is_empty() {
            return Err(EINVAL);
        }
        self.drain(buf, AttachedItems::Take)
    }

    fn drain(&self, buf: &mut [u8], policy: AttachedItems) -> Result<(usize, Vec<P>), Errno> {
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
                Ok((0, Vec::new()))
            } else {
                Err(EAGAIN)
            };
        }
        let taken = inbox.take(buf, policy);
        let peer = peer.clone();
        drop(state);
        // The writer may wait for room in the inbox.
        if let Some(peer) = peer.upgrade() {
            peer.waiters.notify_all();
        }
        Ok(taken)
    }

    pub fn try_write(&self, buf: &[u8]) -> Result<usize, Errno> {
        self.try_sendmsg(buf, Vec::new())
    }

    /// Appends bytes to the peer inbox and attaches `items` to the first of them.
    pub fn try_sendmsg(&self, buf: &[u8], items: Vec<P>) -> Result<usize, Errno> {
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
        let byte_count = inbox.push(buf, items)?;
        drop(peer_state);
        peer.waiters.notify_all();
        Ok(byte_count)
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
            inbox.discard();
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

impl<N: Notify, P: Send + Sync + 'static> Debug for UnixSocket<N, P> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnixSocket")
            .field("state", &self.state.lock().name())
            .finish()
    }
}

impl<N: Notify, P: Send + Sync + 'static> Drop for UnixSocket<N, P> {
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

    static REGISTRY: Registry<Silent, u32> = Registry::new(Silent);

    type Socket = UnixSocket<Silent, u32>;

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

    #[test]
    fn sendmsg_attaches_items_to_first_byte() {
        let (_l, c, s) = pair("/j");
        let mut buf = [0; 8];
        assert_eq!(c.try_sendmsg(b"ab", alloc::vec![7]), Ok(2));
        assert_eq!(s.try_recvmsg(&mut buf), Ok((2, alloc::vec![7])));
        assert_eq!(&buf[..2], b"ab");
    }

    #[test]
    fn recvmsg_stops_before_next_attachment() {
        let (_l, c, s) = pair("/k");
        assert_eq!(c.try_write(b"xy"), Ok(2));
        assert_eq!(c.try_sendmsg(b"z", alloc::vec![1]), Ok(1));
        let mut buf = [0; 8];
        assert_eq!(s.try_recvmsg(&mut buf), Ok((2, alloc::vec![])));
        assert_eq!(&buf[..2], b"xy");
        assert_eq!(s.try_recvmsg(&mut buf), Ok((1, alloc::vec![1])));
        assert_eq!(buf[0], b'z');
    }

    #[test]
    fn plain_read_drops_items() {
        let (_l, c, s) = pair("/l");
        let mut buf = [0; 8];
        assert_eq!(c.try_sendmsg(b"a", alloc::vec![9]), Ok(1));
        assert_eq!(s.try_read(&mut buf), Ok(1));
        assert_eq!(s.try_recvmsg(&mut buf), Err(EAGAIN));
    }

    #[test]
    fn sendmsg_without_bytes_is_invalid() {
        let (_l, c, _s) = pair("/m");
        assert_eq!(c.try_sendmsg(b"", alloc::vec![1]), Err(EINVAL));
    }

    #[test]
    fn recvmsg_with_empty_buffer_is_invalid() {
        let (_l, _c, s) = pair("/n");
        assert_eq!(s.try_recvmsg(&mut []), Err(EINVAL));
    }

    #[test]
    fn shutdown_read_discards_attachments() {
        let (_l, c, s) = pair("/o");
        assert_eq!(c.try_sendmsg(b"a", alloc::vec![3]), Ok(1));
        s.shutdown(ShutdownHow::Read).unwrap();
        assert_eq!(s.try_recvmsg(&mut [0; 4]), Ok((0, alloc::vec![])));
    }

    #[test]
    fn pending_attachment_limit() {
        let (_l, c, _s) = pair("/p");
        for _ in 0..MAX_PENDING_ATTACHED / 16 {
            assert_eq!(c.try_sendmsg(b"a", alloc::vec![0; 16]), Ok(1));
        }
        assert_eq!(c.try_sendmsg(b"a", alloc::vec![0; 16]), Err(ENOBUFS));
    }
}
