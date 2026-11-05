use core::ffi::{c_char, c_int, c_void};
use core::slice;

use kernel_abi::{ENOSYS, EOPNOTSUPP, ShutdownHow};
use libc::{addrinfo, msghdr, nfds_t, pollfd, size_t, sockaddr, socklen_t, ssize_t};

use crate::errno::{fail, ret, set_errno};

#[unsafe(no_mangle)]
pub extern "C" fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int {
    ret(muffin_sys::socket(domain, ty, protocol))
}

/// # Safety
///
/// `addr` must be valid for `len` bytes of reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bind(fd: c_int, addr: *const sockaddr, len: socklen_t) -> c_int {
    ret(unsafe { muffin_sys::bind_raw(fd, addr.cast(), len) }.map(|()| 0))
}

/// # Safety
///
/// `addr` must be valid for `len` bytes of reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn connect(fd: c_int, addr: *const sockaddr, len: socklen_t) -> c_int {
    ret(unsafe { muffin_sys::connect_raw(fd, addr.cast(), len) }.map(|()| 0))
}

#[unsafe(no_mangle)]
pub extern "C" fn listen(fd: c_int, backlog: c_int) -> c_int {
    ret(muffin_sys::listen(fd, backlog).map(|()| 0))
}

/// # Safety
///
/// `addr` and `len` are null, or `len` points to the writable length of `addr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn accept(fd: c_int, addr: *mut sockaddr, len: *mut socklen_t) -> c_int {
    ret(unsafe { muffin_sys::accept_raw(fd, addr.cast(), len) })
}

#[unsafe(no_mangle)]
pub extern "C" fn shutdown(fd: c_int, how: c_int) -> c_int {
    ret(ShutdownHow::try_from(how)
        .and_then(|how| muffin_sys::shutdown(fd, how))
        .map(|()| 0))
}

/// Supports no flags, because the kernel has no `recv` syscall. A plain read
/// serves std's `recv(.., 0)`.
///
/// # Safety
///
/// `buf` must be valid for `len` bytes of writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn recv(fd: c_int, buf: *mut c_void, len: size_t, flags: c_int) -> ssize_t {
    if flags != 0 {
        return fail(EOPNOTSUPP);
    }
    // The kernel rejects an empty buffer with EINVAL, but POSIX reads nothing.
    if len == 0 {
        return 0;
    }
    let buf = unsafe { slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
    ret(muffin_sys::read(fd, buf).map(|n| n as ssize_t))
}

/// Supports no flags, because the kernel has no `send` syscall. A plain write
/// serves std's `send(.., 0)`.
///
/// # Safety
///
/// `buf` must be valid for `len` bytes of reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn send(fd: c_int, buf: *const c_void, len: size_t, flags: c_int) -> ssize_t {
    if flags != 0 {
        return fail(EOPNOTSUPP);
    }
    // The kernel rejects an empty buffer with EINVAL, but POSIX writes nothing.
    if len == 0 {
        return 0;
    }
    let buf = unsafe { slice::from_raw_parts(buf.cast::<u8>(), len) };
    ret(muffin_sys::write(fd, buf).map(|n| n as ssize_t))
}

/// Supports no flags, as `send` does.
///
/// # Safety
///
/// `msg` and every buffer it names must be valid for reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sendmsg(fd: c_int, msg: *const msghdr, flags: c_int) -> ssize_t {
    ret(unsafe { muffin_sys::sendmsg_raw(fd, msg.cast(), flags) }.map(|n| n as ssize_t))
}

/// Supports no flags, as `recv` does.
///
/// # Safety
///
/// `msg` and every buffer it names must be valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn recvmsg(fd: c_int, msg: *mut msghdr, flags: c_int) -> ssize_t {
    ret(unsafe { muffin_sys::recvmsg_raw(fd, msg.cast(), flags) }.map(|n| n as ssize_t))
}

// shortcut: kernel has no syscall for these; upgrade when
// `kernel/abi/src/syscall.rs` adds one.
enosys! {
    fn getsockname(c_int, *mut sockaddr, *mut socklen_t) -> c_int;
    fn getpeername(c_int, *mut sockaddr, *mut socklen_t) -> c_int;
    fn setsockopt(c_int, c_int, c_int, *const c_void, socklen_t) -> c_int;
    fn getsockopt(c_int, c_int, c_int, *mut c_void, *mut socklen_t) -> c_int;
    fn socketpair(c_int, c_int, c_int, *mut c_int) -> c_int;
    fn sendto(c_int, *const c_void, size_t, c_int, *const sockaddr, socklen_t) -> ssize_t;
    fn recvfrom(c_int, *mut c_void, size_t, c_int, *mut sockaddr, *mut socklen_t) -> ssize_t;
    fn poll(*mut pollfd, nfds_t, c_int) -> c_int;
}

/// Returns the EAI code, not `-1`, because that is the `getaddrinfo` convention.
#[unsafe(no_mangle)]
pub extern "C" fn getaddrinfo(
    _: *const c_char,
    _: *const c_char,
    _: *const addrinfo,
    _: *mut *mut addrinfo,
) -> c_int {
    set_errno(ENOSYS);
    libc::EAI_SYSTEM
}

/// Does nothing, because `getaddrinfo` never allocates a result.
#[unsafe(no_mangle)]
pub extern "C" fn freeaddrinfo(_: *mut addrinfo) {}

#[unsafe(no_mangle)]
pub extern "C" fn gai_strerror(_: c_int) -> *const c_char {
    c"address lookup is unsupported".as_ptr()
}
