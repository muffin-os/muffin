use alloc::vec::Vec;
use core::ffi::c_int;
use core::mem::size_of;

use kernel_abi::{
    AF_UNIX, CmsgHdr, EAFNOSUPPORT, EINVAL, EOPNOTSUPP, EPROTONOSUPPORT, EPROTOTYPE, Errno,
    MSG_CTRUNC, MsgHdr, SOCK_STREAM, ShutdownHow,
};
use tracing::{Level, instrument};

use crate::access::{CwdAccess, SocketAccess};

const FD_BYTES: usize = CmsgHdr::FD_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvOutcome {
    pub len: usize,
    pub controllen: usize,
    pub msg_flags: i32,
}

impl RecvOutcome {
    pub fn write_to(&self, hdr: &mut MsgHdr) {
        hdr.msg_controllen = self.controllen;
        hdr.msg_flags = self.msg_flags;
    }
}

/// Creates an unbound AF_UNIX stream socket and returns its descriptor number.
///
/// # Errors
/// Returns `EAFNOSUPPORT`, `EPROTOTYPE` or `EPROTONOSUPPORT` for an unsupported
/// domain, type or protocol. The checks run in that order.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_socket<Cx: SocketAccess>(
    cx: &Cx,
    domain: c_int,
    ty: c_int,
    protocol: c_int,
) -> Result<usize, Errno> {
    if domain != c_int::from(AF_UNIX) {
        return Err(EAFNOSUPPORT);
    }
    if ty != SOCK_STREAM {
        return Err(EPROTOTYPE);
    }
    if protocol != 0 {
        return Err(EPROTONOSUPPORT);
    }
    Ok(Into::<c_int>::into(cx.socket()) as usize)
}

/// Binds `fd` to `path`. A relative path joins the working directory.
///
/// # Errors
/// Returns any error of [`SocketAccess::bind`].
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_bind<Cx: CwdAccess + SocketAccess>(
    cx: &Cx,
    fd: Cx::Fd,
    path: &str,
) -> Result<usize, Errno> {
    cx.bind(fd, cx.resolve(path))?;
    Ok(0)
}

/// Marks `fd` as listening with a queue of `backlog` pending connections.
///
/// # Errors
/// Returns any error of [`SocketAccess::listen`].
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_listen<Cx: SocketAccess>(cx: &Cx, fd: Cx::Fd, backlog: usize) -> Result<usize, Errno> {
    cx.listen(fd, backlog)?;
    Ok(0)
}

/// Waits for a pending connection on `fd` and returns its descriptor number.
///
/// # Errors
/// Returns any error of [`SocketAccess::accept`].
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_accept<Cx: SocketAccess>(cx: &Cx, fd: Cx::Fd) -> Result<usize, Errno> {
    Ok(Into::<c_int>::into(cx.accept(fd)?) as usize)
}

/// Connects `fd` to the listener at `path`. Resolves the path like [`sys_bind`].
///
/// # Errors
/// Returns any error of [`SocketAccess::connect`].
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_connect<Cx: CwdAccess + SocketAccess>(
    cx: &Cx,
    fd: Cx::Fd,
    path: &str,
) -> Result<usize, Errno> {
    cx.connect(fd, cx.resolve(path))?;
    Ok(0)
}

/// Shuts down one or both directions of the connection on `fd`.
///
/// # Errors
/// Returns any error of [`SocketAccess::shutdown`].
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_shutdown<Cx: SocketAccess>(
    cx: &Cx,
    fd: Cx::Fd,
    how: ShutdownHow,
) -> Result<usize, Errno> {
    cx.shutdown(fd, how)?;
    Ok(0)
}

/// Sends `data` on `fd` and attaches the `SCM_RIGHTS` descriptors of `control`
/// to its first byte. Returns the byte count sent.
///
/// # Errors
/// Returns `EOPNOTSUPP` for a non-zero `flags`, `EINVAL` for a malformed control
/// buffer, and any error of [`SocketAccess::sendmsg`].
#[instrument(level = Level::TRACE, skip(cx, data, control))]
pub fn sys_sendmsg<Cx: SocketAccess>(
    cx: &Cx,
    fd: Cx::Fd,
    flags: c_int,
    data: &[u8],
    control: &[u8],
) -> Result<usize, Errno> {
    if flags != 0 {
        return Err(EOPNOTSUPP);
    }
    let fds = parse_scm_rights::<Cx::Fd>(control)?;
    cx.sendmsg(fd, data, &fds)
}

/// Receives into `buf` and encodes the installed descriptors into `control`.
/// Descriptors that do not fit `control` are closed and `MSG_CTRUNC` is set.
///
/// # Errors
/// Returns `EOPNOTSUPP` for a non-zero `flags` and any error of
/// [`SocketAccess::recvmsg`].
#[instrument(level = Level::TRACE, skip(cx, buf, control))]
pub fn sys_recvmsg<Cx: SocketAccess>(
    cx: &Cx,
    fd: Cx::Fd,
    flags: c_int,
    buf: &mut [u8],
    control: &mut [u8],
) -> Result<RecvOutcome, Errno> {
    if flags != 0 {
        return Err(EOPNOTSUPP);
    }
    let max_fds = scm_rights_capacity(control.len());
    let received = cx.recvmsg(fd, buf, max_fds)?;
    let controllen = encode_scm_rights(&received.fds, control);
    Ok(RecvOutcome {
        len: received.len,
        controllen,
        msg_flags: if received.truncated { MSG_CTRUNC } else { 0 },
    })
}

/// Bytes after the first header are ignored.
fn parse_scm_rights<Fd: From<c_int>>(control: &[u8]) -> Result<Vec<Fd>, Errno> {
    if control.is_empty() {
        return Ok(Vec::new());
    }
    if control.len() < size_of::<CmsgHdr>() {
        return Err(EINVAL);
    }
    // SAFETY: `control.len() >= size_of::<CmsgHdr>()` by the check above, and
    // every bit pattern is a valid `CmsgHdr`; `read_unaligned` needs no alignment.
    let hdr = unsafe { control.as_ptr().cast::<CmsgHdr>().read_unaligned() };
    hdr.scm_rights_count(control.len()).ok_or(EINVAL)?;
    Ok(control[CmsgHdr::len(0)..hdr.cmsg_len]
        .as_chunks::<FD_BYTES>()
        .0
        .iter()
        .map(|raw| Fd::from(c_int::from_ne_bytes(*raw)))
        .collect())
}

fn scm_rights_capacity(controllen: usize) -> usize {
    controllen
        .checked_sub(CmsgHdr::len(0))
        .map_or(0, |data| data / FD_BYTES)
}

fn encode_scm_rights<Fd: Into<c_int> + Clone>(fds: &[Fd], control: &mut [u8]) -> usize {
    assert!(
        fds.len() <= scm_rights_capacity(control.len()),
        "descriptors beyond the control capacity must be closed before encoding"
    );
    if fds.is_empty() {
        return 0;
    }
    let data_len = fds.len() * FD_BYTES;
    // SAFETY: `fds` is non-empty and at most `scm_rights_capacity(control.len())`,
    // so `control.len() >= size_of::<CmsgHdr>()`; `write_unaligned` needs no
    // alignment.
    unsafe {
        control
            .as_mut_ptr()
            .cast::<CmsgHdr>()
            .write_unaligned(CmsgHdr::scm_rights(fds.len()));
    }
    for (slot, fd) in control[CmsgHdr::len(0)..]
        .as_chunks_mut::<FD_BYTES>()
        .0
        .iter_mut()
        .zip(fds)
    {
        *slot = Into::<c_int>::into(fd.clone()).to_ne_bytes();
    }
    CmsgHdr::space(data_len).min(control.len())
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ffi::c_int;

    use kernel_abi::{
        AF_UNIX, CmsgHdr, EAFNOSUPPORT, EINVAL, EOPNOTSUPP, EPROTONOSUPPORT, EPROTOTYPE, Errno,
        MSG_CTRUNC, SOCK_STREAM, ShutdownHow,
    };
    use kernel_vfs::path::AbsoluteOwnedPath;
    use spin::mutex::Mutex;
    use spin::rwlock::RwLock;

    use super::{
        encode_scm_rights, parse_scm_rights, scm_rights_capacity, sys_bind, sys_recvmsg,
        sys_sendmsg, sys_socket,
    };
    use crate::access::{CwdAccess, Received, SocketAccess};

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Bind(i32, AbsoluteOwnedPath),
        Listen(i32, usize),
        Connect(i32, AbsoluteOwnedPath),
        SendMsg(i32, Vec<u8>, Vec<i32>),
        RecvMsg(i32, usize),
    }

    struct TestCx {
        cwd: RwLock<AbsoluteOwnedPath>,
        calls: Mutex<Vec<Call>>,
        recv_is_truncated: bool,
    }

    impl TestCx {
        fn new(cwd: &str) -> Self {
            Self {
                cwd: AbsoluteOwnedPath::try_from(cwd).unwrap().into(),
                calls: Mutex::new(Vec::new()),
                recv_is_truncated: false,
            }
        }
    }

    impl CwdAccess for TestCx {
        fn current_working_directory(&self) -> &RwLock<AbsoluteOwnedPath> {
            &self.cwd
        }
    }

    impl SocketAccess for TestCx {
        type Fd = i32;

        fn socket(&self) -> i32 {
            3
        }

        fn bind(&self, fd: i32, path: AbsoluteOwnedPath) -> Result<(), Errno> {
            self.calls.lock().push(Call::Bind(fd, path));
            Ok(())
        }

        fn listen(&self, fd: i32, backlog: usize) -> Result<(), Errno> {
            self.calls.lock().push(Call::Listen(fd, backlog));
            Ok(())
        }

        fn accept(&self, _fd: i32) -> Result<i32, Errno> {
            Ok(4)
        }

        fn connect(&self, fd: i32, path: AbsoluteOwnedPath) -> Result<(), Errno> {
            self.calls.lock().push(Call::Connect(fd, path));
            Ok(())
        }

        fn shutdown(&self, _fd: i32, _how: ShutdownHow) -> Result<(), Errno> {
            Ok(())
        }

        fn sendmsg(&self, fd: i32, data: &[u8], fds: &[i32]) -> Result<usize, Errno> {
            self.calls
                .lock()
                .push(Call::SendMsg(fd, data.to_vec(), fds.to_vec()));
            Ok(data.len())
        }

        fn recvmsg(&self, fd: i32, buf: &mut [u8], max_fds: usize) -> Result<Received<i32>, Errno> {
            self.calls.lock().push(Call::RecvMsg(fd, max_fds));
            buf[0] = b'r';
            Ok(Received {
                len: 1,
                fds: vec![5; max_fds],
                truncated: self.recv_is_truncated,
            })
        }
    }

    #[test]
    fn socket_checks_domain_type_protocol_in_order() {
        let cx = TestCx::new("/");
        let unix = c_int::from(AF_UNIX);
        assert_eq!(sys_socket(&cx, 2, 2, 1), Err(EAFNOSUPPORT));
        assert_eq!(sys_socket(&cx, unix, 2, 1), Err(EPROTOTYPE));
        assert_eq!(sys_socket(&cx, unix, SOCK_STREAM, 1), Err(EPROTONOSUPPORT));
        assert_eq!(sys_socket(&cx, unix, SOCK_STREAM, 0), Ok(3));
    }

    #[test]
    fn bind_joins_relative_path_to_cwd() {
        let cx = TestCx::new("/tmp");
        assert_eq!(sys_bind(&cx, 3, "sock"), Ok(0));
        assert_eq!(
            *cx.calls.lock(),
            vec![Call::Bind(
                3,
                AbsoluteOwnedPath::try_from("/tmp/sock").unwrap()
            )]
        );
    }

    #[test]
    fn parse_scm_rights_accepts_an_empty_buffer() {
        assert_eq!(parse_scm_rights::<i32>(&[]), Ok(vec![]));
    }

    #[test]
    fn parse_scm_rights_rejects_other_types() {
        let mut control = [0u8; CmsgHdr::space(4)];
        assert_eq!(encode_scm_rights(&[3], &mut control), CmsgHdr::space(4));
        let type_offset = core::mem::offset_of!(CmsgHdr, cmsg_type);
        control[type_offset..type_offset + 4].copy_from_slice(&2i32.to_ne_bytes());
        assert_eq!(parse_scm_rights::<i32>(&control), Err(EINVAL));
    }

    #[test]
    fn encode_then_parse_round_trips() {
        let mut control = [0u8; CmsgHdr::space(8)];
        assert_eq!(encode_scm_rights(&[3, 4], &mut control), CmsgHdr::space(8));
        assert_eq!(parse_scm_rights::<i32>(&control), Ok(vec![3, 4]));
    }

    #[test]
    fn capacity_counts_whole_descriptors() {
        assert_eq!(scm_rights_capacity(0), 0);
        assert_eq!(scm_rights_capacity(CmsgHdr::len(0)), 0);
        assert_eq!(scm_rights_capacity(CmsgHdr::len(4)), 1);
        // The pad of `CMSG_SPACE(4)` holds a second descriptor, as on Linux.
        assert_eq!(scm_rights_capacity(CmsgHdr::space(4)), 2);
    }

    #[test]
    fn sendmsg_rejects_flags_and_forwards_descriptors() {
        let cx = TestCx::new("/");
        assert_eq!(sys_sendmsg(&cx, 3, 1, b"x", &[]), Err(EOPNOTSUPP));
        let mut control = [0u8; CmsgHdr::space(4)];
        encode_scm_rights(&[7], &mut control);
        assert_eq!(sys_sendmsg(&cx, 3, 0, b"xy", &control), Ok(2));
        assert_eq!(
            *cx.calls.lock(),
            vec![Call::SendMsg(3, b"xy".to_vec(), vec![7])]
        );
    }

    #[test]
    fn recvmsg_encodes_descriptors_and_reports_truncation() {
        let mut cx = TestCx::new("/");
        let mut buf = [0u8; 4];
        let mut control = [0u8; CmsgHdr::len(4)];
        let outcome = sys_recvmsg(&cx, 3, 0, &mut buf, &mut control).unwrap();
        assert_eq!(outcome.len, 1);
        assert_eq!(outcome.controllen, CmsgHdr::len(4));
        assert_eq!(outcome.msg_flags, 0);
        assert_eq!(buf[0], b'r');
        assert_eq!(parse_scm_rights::<i32>(&control), Ok(vec![5]));
        assert_eq!(*cx.calls.lock(), vec![Call::RecvMsg(3, 1)]);

        cx.recv_is_truncated = true;
        let outcome = sys_recvmsg(&cx, 3, 0, &mut buf, &mut control).unwrap();
        assert_eq!(outcome.msg_flags, MSG_CTRUNC);
        assert_eq!(
            sys_recvmsg(&cx, 3, 1, &mut buf, &mut control),
            Err(EOPNOTSUPP)
        );
    }

    #[test]
    fn recvmsg_without_control_room_installs_nothing() {
        let cx = TestCx::new("/");
        let mut buf = [0u8; 4];
        let outcome = sys_recvmsg(&cx, 3, 0, &mut buf, &mut []).unwrap();
        assert_eq!(outcome.controllen, 0);
        assert_eq!(*cx.calls.lock(), vec![Call::RecvMsg(3, 0)]);
    }
}
