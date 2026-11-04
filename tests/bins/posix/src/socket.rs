use core::ffi::c_int;

use minilib::{
    AF_UNIX, EADDRINUSE, EAFNOSUPPORT, ECONNREFUSED, EDESTADDRREQ, EINVAL, ENOTCONN, ENOTSOCK,
    ENOTTY, EPROTONOSUPPORT, EPROTOTYPE, ESPIPE, FbScreenInfo, IoctlRequest, O_RDONLY, S_IFMT,
    S_IFSOCK, SOCK_STREAM, ShutdownHow, Stat, Whence, accept, bind, bind_raw, close, connect,
    fstat, fsync, ioctl, listen, lseek, open, read, shutdown, socket, write,
};

use crate::check;

const UNSUPPORTED_FAMILY: c_int = 2;

const SOCKET_PATH: &str = "/posix-sock";

pub fn run() {
    check::group("socket");

    check::expect_err(
        "socket/unsupported_family",
        socket(UNSUPPORTED_FAMILY, SOCK_STREAM, 0),
        EAFNOSUPPORT,
    );
    check::expect_err(
        "socket/unsupported_type",
        socket(AF_UNIX as c_int, 2, 0),
        EPROTOTYPE,
    );
    check::expect_err(
        "socket/unsupported_protocol",
        socket(AF_UNIX as c_int, SOCK_STREAM, 1),
        EPROTONOSUPPORT,
    );

    let hello = check::unwrap_or_fail("socket/open_hello", open("/data/hello.txt", O_RDONLY));
    check::expect_err("socket/bind_regular_file", bind(hello, "/x"), ENOTSOCK);
    check::expect_ok("socket/close_hello", close(hello), ());

    let s = check::unwrap_or_fail("socket/create", socket(AF_UNIX as c_int, SOCK_STREAM, 0));

    let wrong_family = [UNSUPPORTED_FAMILY as u8, 0, b'/', b'a', 0];
    // Safety: each address lives on the stack for the whole call. The kernel
    // must reject the oversized length before it reads.
    check::expect_err(
        "socket/bind_wrong_family",
        unsafe { bind_raw(s, wrong_family.as_ptr(), 5) },
        EAFNOSUPPORT,
    );
    let addr = [AF_UNIX as u8, 0, b'/', b'a', 0];
    check::expect_err(
        "socket/bind_len_below_family",
        unsafe { bind_raw(s, addr.as_ptr(), 1) },
        EINVAL,
    );
    check::expect_err(
        "socket/bind_empty_path",
        unsafe { bind_raw(s, addr.as_ptr(), 2) },
        EINVAL,
    );
    check::expect_err(
        "socket/bind_len_above_sockaddr",
        unsafe { bind_raw(s, addr.as_ptr(), 111) },
        EINVAL,
    );

    let mut one = [0_u8; 1];
    check::expect_err("socket/listen_unbound", listen(s, 1), EDESTADDRREQ);
    check::expect_err("socket/accept_unbound", accept(s), EINVAL);
    check::expect_err("socket/read_unconnected", read(s, &mut one), ENOTCONN);
    check::expect_err("socket/write_unconnected", write(s, b"x"), ENOTCONN);
    check::expect_err(
        "socket/shutdown_unconnected",
        shutdown(s, ShutdownHow::Both),
        ENOTCONN,
    );
    check::expect_err("socket/connect_missing", connect(s, "/nope"), ECONNREFUSED);

    check::expect_ok("socket/bind", bind(s, SOCKET_PATH), ());
    let t = check::unwrap_or_fail(
        "socket/create_second",
        socket(AF_UNIX as c_int, SOCK_STREAM, 0),
    );
    check::expect_err("socket/bind_in_use", bind(t, SOCKET_PATH), EADDRINUSE);
    check::expect_err(
        "socket/connect_not_listening",
        connect(t, SOCKET_PATH),
        ECONNREFUSED,
    );
    check::expect_err("socket/bind_twice", bind(s, "/other"), EINVAL);

    check::expect_err("socket/lseek", lseek(s, 0, Whence::Set), ESPIPE);
    let mut stat = Stat::default();
    check::expect_ok("socket/fstat", fstat(s, &mut stat), ());
    check::require("socket/fstat_mode", stat.mode & S_IFMT == S_IFSOCK);
    let mut screen = FbScreenInfo::default();
    check::expect_err(
        "socket/ioctl",
        ioctl(s, IoctlRequest::FbGetScreenInfo, &mut screen),
        ENOTTY,
    );
    check::expect_err("socket/fsync", fsync(s), EINVAL);

    check::expect_ok("socket/close", close(s), ());
    check::expect_ok("socket/bind_after_close", bind(t, SOCKET_PATH), ());
    check::expect_ok("socket/close_second", close(t), ());
}
