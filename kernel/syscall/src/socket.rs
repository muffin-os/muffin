use core::ffi::c_int;

use kernel_abi::{
    AF_UNIX, EAFNOSUPPORT, EPROTONOSUPPORT, EPROTOTYPE, Errno, SOCK_STREAM, ShutdownHow,
};
use tracing::{Level, instrument};

use crate::access::{CwdAccess, SocketAccess};

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

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ffi::c_int;

    use kernel_abi::{
        AF_UNIX, EAFNOSUPPORT, EPROTONOSUPPORT, EPROTOTYPE, Errno, SOCK_STREAM, ShutdownHow,
    };
    use kernel_vfs::path::AbsoluteOwnedPath;
    use spin::mutex::Mutex;
    use spin::rwlock::RwLock;

    use super::{sys_bind, sys_socket};
    use crate::access::{CwdAccess, SocketAccess};

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Bind(i32, AbsoluteOwnedPath),
        Listen(i32, usize),
        Connect(i32, AbsoluteOwnedPath),
    }

    struct TestCx {
        cwd: RwLock<AbsoluteOwnedPath>,
        calls: Mutex<Vec<Call>>,
    }

    impl TestCx {
        fn new(cwd: &str) -> Self {
            Self {
                cwd: AbsoluteOwnedPath::try_from(cwd).unwrap().into(),
                calls: Mutex::new(Vec::new()),
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
}
