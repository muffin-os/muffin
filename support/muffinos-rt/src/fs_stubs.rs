// shortcut: kernel has no syscall for these; upgrade when
// `kernel/abi/src/syscall.rs` adds one.

use core::ffi::{c_char, c_int};

use kernel_abi::ENOSYS;
use libc::{mode_t, off_t, ssize_t, timespec};

use crate::errno::set_errno;

enosys! {
    fn rmdir(*const c_char) -> c_int;
    fn rename(*const c_char, *const c_char) -> c_int;
    fn mkdir(*const c_char, mode_t) -> c_int;
    fn chdir(*const c_char) -> c_int;
    fn readlink(*const c_char, *mut c_char, usize) -> ssize_t;
    fn realpath(*const c_char, *mut c_char) -> *mut c_char;
    fn symlink(*const c_char, *const c_char) -> c_int;
    fn link(*const c_char, *const c_char) -> c_int;
    fn chmod(*const c_char, mode_t) -> c_int;
    fn fchmod(c_int, mode_t) -> c_int;
    fn truncate(*const c_char, off_t) -> c_int;
    fn utimensat(c_int, *const c_char, *const timespec, c_int) -> c_int;
    fn futimens(c_int, *const timespec) -> c_int;
    fn dup(c_int) -> c_int;
    fn dup2(c_int, c_int) -> c_int;
    fn pipe(*mut c_int) -> c_int;
    fn pipe2(*mut c_int, c_int) -> c_int;
}

/// Returns `0`, the "not a terminal" result, with errno `ENOSYS`.
#[unsafe(no_mangle)]
pub extern "C" fn isatty(_: c_int) -> c_int {
    set_errno(ENOSYS);
    0
}
