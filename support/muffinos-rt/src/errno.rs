use core::ffi::c_int;
use core::ptr;

use kernel_abi::Errno;

#[thread_local]
static mut ERRNO: c_int = 0;

/// This function returns the address of the errno of the calling thread.
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
    // SAFETY: `ERRNO` is thread-local, and no reference to it outlives a call.
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

/// This function converts a result to the pthread convention. The convention
/// is 0 for success, or the error number.
pub(crate) fn error_code(result: Result<(), Errno>) -> c_int {
    result.map_or_else(c_int::from, |()| 0)
}
