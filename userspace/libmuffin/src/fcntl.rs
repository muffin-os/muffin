//! fcntl.h

use crate::unimplemented_function;
use kernel_abi::{SYS_FCNTL, SYS_OPEN};
use libc::{c_char, c_int};

#[unsafe(no_mangle)]
#[allow(unused_mut)]
pub unsafe extern "C" fn open(path: *const c_char, oflag: c_int, mut varargs: ...) -> c_int {
    unimplemented_function(SYS_OPEN)
}

#[unsafe(no_mangle)]
#[allow(unused_mut)]
pub unsafe extern "C" fn fcntl(fildes: c_int, cmd: c_int, mut varargs: ...) -> c_int {
    unimplemented_function(SYS_FCNTL)
}
