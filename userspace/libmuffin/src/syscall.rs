#![allow(dead_code)] // TODO: remove

#[cfg(target_os = "muffin")]
use core::arch::asm;

use kernel_abi::Errno;

pub struct Syscall;

impl Syscall {
    pub fn open(path: &[u8], oflag: usize, mode: usize) -> Result<usize, Errno> {
        let result = syscall4(
            kernel_abi::SYS_OPEN,
            path.as_ptr() as usize,
            path.len(),
            oflag,
            mode,
        );
        if result < 0 {
            Err(Errno::from(-result))
        } else {
            Ok(result as usize)
        }
    }

    pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize, Errno> {
        let result = syscall3(
            kernel_abi::SYS_READ,
            fd,
            buf.as_mut_ptr() as usize,
            buf.len(),
        );
        if result < 0 {
            Err(Errno::from(-result))
        } else {
            Ok(result as usize)
        }
    }

    pub fn write(fd: usize, buf: &[u8]) -> Result<usize, Errno> {
        let result = syscall3(kernel_abi::SYS_WRITE, fd, buf.as_ptr() as usize, buf.len());
        if result < 0 {
            Err(Errno::from(-result))
        } else {
            Ok(result as usize)
        }
    }
}

#[cfg(not(target_os = "muffin"))]
pub(crate) fn syscall1(_number: usize, _arg1: usize) -> isize {
    panic!("syscall1 is not implemented for this target OS");
}

#[cfg(not(target_os = "muffin"))]
pub(crate) fn syscall2(_number: usize, _arg1: usize, _arg2: usize) -> isize {
    panic!("syscall2 is not implemented for this target OS");
}

#[cfg(not(target_os = "muffin"))]
pub(crate) fn syscall3(_number: usize, _arg1: usize, _arg2: usize, _arg3: usize) -> isize {
    panic!("syscall3 is not implemented for this target OS");
}

#[cfg(not(target_os = "muffin"))]
pub(crate) fn syscall4(
    _number: usize,
    _arg1: usize,
    _arg2: usize,
    _arg3: usize,
    _arg4: usize,
) -> isize {
    panic!("syscall4 is not implemented for this target OS");
}

#[cfg(not(target_os = "muffin"))]
pub(crate) fn syscall5(
    _number: usize,
    _arg1: usize,
    _arg2: usize,
    _arg3: usize,
    _arg4: usize,
    _arg5: usize,
) -> isize {
    panic!("syscall5 is not implemented for this target OS");
}

#[cfg(not(target_os = "muffin"))]
pub(crate) fn syscall6(
    _number: usize,
    _arg1: usize,
    _arg2: usize,
    _arg3: usize,
    _arg4: usize,
    _arg5: usize,
    _arg6: usize,
) -> isize {
    panic!("syscall6 is not implemented for this target OS");
}

/// Perform a system call with a single argument.
///
/// This function is intended to be used for making a system call
/// to the kernel, and is not POSIX compliant, meaning that it
/// does not modify `errno` on failure.
///
/// If you use this, you must
/// handle the return value and any errors yourself. This includes
/// emulating behavior that POSIX specifies.
#[cfg(target_os = "muffin")]
pub(crate) fn syscall1(number: usize, arg1: usize) -> isize {
    let result: isize;
    unsafe {
        asm!(
        "int 0x80",
        in("rax") number,
        in("rdi") arg1,
        lateout("rax") result,
        );
    }
    result
}

/// Perform a system call with two arguments.
///
/// This function is intended to be used for making a system call
/// to the kernel, and is not POSIX compliant, meaning that it
/// does not modify `errno` on failure.
///
/// If you use this, you must
/// handle the return value and any errors yourself. This includes
/// emulating behavior that POSIX specifies.
#[cfg(target_os = "muffin")]
pub(crate) fn syscall2(number: usize, arg1: usize, arg2: usize) -> isize {
    let result: isize;
    unsafe {
        asm!(
        "int 0x80",
        in("rax") number,
        in("rdi") arg1,
        in("rsi") arg2,
        lateout("rax") result,
        );
    }
    result
}

/// Perform a system call with three arguments.
///
/// This function is intended to be used for making a system call
/// to the kernel, and is not POSIX compliant, meaning that it
/// does not modify `errno` on failure.
///
/// If you use this, you must
/// handle the return value and any errors yourself. This includes
/// emulating behavior that POSIX specifies.
#[cfg(target_os = "muffin")]
pub(crate) fn syscall3(number: usize, arg1: usize, arg2: usize, arg3: usize) -> isize {
    let result: isize;
    unsafe {
        asm!(
        "int 0x80",
        in("rax") number,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        lateout("rax") result,
        );
    }
    result
}

/// Perform a system call with four arguments.
///
/// This function is intended to be used for making a system call
/// to the kernel, and is not POSIX compliant, meaning that it
/// does not modify `errno` on failure.
///
/// If you use this, you must
/// handle the return value and any errors yourself. This includes
/// emulating behavior that POSIX specifies.
#[cfg(target_os = "muffin")]
pub(crate) fn syscall4(number: usize, arg1: usize, arg2: usize, arg3: usize, arg4: usize) -> isize {
    let result: isize;
    unsafe {
        asm!(
        "int 0x80",
        in("rax") number,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("rcx") arg4,
        lateout("rax") result,
        );
    }
    result
}

/// Perform a system call with five arguments.
///
/// This function is intended to be used for making a system call
/// to the kernel, and is not POSIX compliant, meaning that it
/// does not modify `errno` on failure.
///
/// If you use this, you must
/// handle the return value and any errors yourself. This includes
/// emulating behavior that POSIX specifies.
#[cfg(target_os = "muffin")]
pub(crate) fn syscall5(
    number: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> isize {
    let result: isize;
    unsafe {
        asm!(
        "int 0x80",
        in("rax") number,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("rcx") arg4,
        in("r8") arg5,
        lateout("rax") result,
        );
    }
    result
}

/// Perform a system call with six arguments.
///
/// This function is intended to be used for making a system call
/// to the kernel, and is not POSIX compliant, meaning that it
/// does not modify `errno` on failure.
///
/// If you use this, you must
/// handle the return value and any errors yourself. This includes
/// emulating behavior that POSIX specifies.
#[cfg(target_os = "muffin")]
pub(crate) fn syscall6(
    number: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> isize {
    let result: isize;
    unsafe {
        asm!(
        "int 0x80",
        in("rax") number,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("rcx") arg4,
        in("r8") arg5,
        in("r9") arg6,
        lateout("rax") result,
        );
    }
    result
}
