//! C runtime that the patched `libc` crate and `std` link against on muffin.
//!
//! The crate exports `_start`, `environ`, `__errno_location` and the C library
//! functions that std calls. Each function that has a kernel syscall forwards
//! to `muffin_sys`. Functions the kernel cannot serve fail with `ENOSYS`. The
//! process has one thread, so the pthread functions keep their state without
//! atomics, and each pthread function that needs a second thread returns a
//! fixed code.
//!
//! On failure, the C functions set errno and return `-1` or a null pointer.
//! `posix_memalign`, `strerror_r` and the pthread functions return the error
//! number instead.
#![cfg_attr(not(test), no_std)]
// `open`, `fcntl` and `ioctl` take C variadic arguments.
#![cfg_attr(not(test), feature(c_variadic))]
// Host unit tests build only the modules with pure logic, without their C
// symbols. Those symbols would replace the host libc ones in the test binary.
#![cfg_attr(test, allow(dead_code))]

/// Defines C functions that fail with `ENOSYS` and ignore their arguments.
#[cfg(not(test))]
macro_rules! enosys {
    ($(fn $name:ident($($arg:ty),*) -> $ret:ty;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $name($(_: $arg),*) -> $ret {
                $crate::errno::fail(kernel_abi::ENOSYS)
            }
        )*
    };
}

mod env;
#[cfg(not(test))]
mod env_stubs;
mod errno;
#[cfg(not(test))]
mod fs;
#[cfg(not(test))]
mod fs_stubs;
mod mem;
#[cfg(not(test))]
mod process;
#[cfg(not(test))]
mod start;
mod string;
#[cfg(not(test))]
mod thread;
#[cfg(not(test))]
mod time;

pub use muffin_sys::exe_path;
