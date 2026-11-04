use core::ffi::c_int;
use core::ptr;

use kernel_abi::Errno;

static mut ERRNO: c_int = 0;

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __errno_location() -> *mut c_int {
    &raw mut ERRNO
}

/// Value a C function returns on failure, after it sets errno.
pub(crate) trait Failure {
    const FAILURE: Self;
}

impl Failure for i32 {
    const FAILURE: Self = -1;
}

impl Failure for i64 {
    const FAILURE: Self = -1;
}

impl Failure for isize {
    const FAILURE: Self = -1;
}

impl<T> Failure for *mut T {
    const FAILURE: Self = ptr::null_mut();
}

pub(crate) fn set_errno(errno: Errno) {
    // SAFETY: the process has one thread, so no other access to `ERRNO` runs.
    unsafe { ERRNO = errno.into() };
}

/// Sets errno and returns the failure value of `R`.
pub(crate) fn fail<R: Failure>(errno: Errno) -> R {
    set_errno(errno);
    R::FAILURE
}

/// Converts a result to the C convention: the value, or the failure value of
/// `R` with errno set.
pub(crate) fn ret<R: Failure>(result: Result<R, Errno>) -> R {
    result.unwrap_or_else(fail)
}
