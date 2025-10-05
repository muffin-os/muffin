use core::ops::Neg;
use core::slice::{from_raw_parts, from_raw_parts_mut};

use access::KernelAccess;
use kernel_abi::{EINVAL, Errno, syscall_name};
use kernel_syscall::access::FileAccess;
use kernel_syscall::fcntl::sys_open;
use kernel_syscall::mman::sys_mmap;
use kernel_syscall::unistd::{sys_getcwd, sys_read, sys_write};
use kernel_syscall::{UserspaceMutPtr, UserspacePtr};
use log::{error, trace};
use x86_64::instructions::hlt;

mod access;

#[must_use]
pub fn dispatch_syscall(
    n: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> isize {
    trace!(
        "syscall: {} ({n}) {arg1} {arg2} {arg3} {arg4} {arg5} {arg6}",
        syscall_name(n)
    );

    let result = match n {
        kernel_abi::SYS_GETCWD => dispatch_sys_getcwd(arg1, arg2),
        kernel_abi::SYS_MMAP => dispatch_sys_mmap(arg1, arg2, arg3, arg4, arg5, arg6),
        kernel_abi::SYS_OPEN => dispatch_sys_open(arg1, arg2, arg3, arg4),
        kernel_abi::SYS_READ => dispatch_sys_read(arg1, arg2, arg3),
        kernel_abi::SYS_WRITE => dispatch_sys_write(arg1, arg2, arg3),
        _ => {
            error!("unimplemented syscall: {} ({n})", syscall_name(n));
            loop {
                hlt();
            }
        }
    };

    match result {
        Ok(ret) => {
            trace!("syscall {} ({n}) returned {ret}", syscall_name(n));
            ret as isize
        }
        Err(e) => {
            error!("syscall {} ({n}) failed with error: {e:?}", syscall_name(n));
            Into::<isize>::into(e).neg()
        }
    }
}

unsafe fn slice_from_ptr_and_len<'a, T>(ptr: usize, len: usize) -> Result<&'a [T], Errno> {
    if ptr == 0 || len == 0 {
        return Err(EINVAL);
    }
    let slice = unsafe { from_raw_parts(ptr as *mut T, len) };
    Ok(slice)
}

unsafe fn slice_from_ptr_and_len_mut<'a, T>(ptr: usize, len: usize) -> Result<&'a mut [T], Errno> {
    if ptr == 0 || len == 0 {
        return Err(EINVAL);
    }
    let slice = unsafe { from_raw_parts_mut(ptr as *mut T, len) };
    Ok(slice)
}

fn dispatch_sys_getcwd(path: usize, size: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let path = unsafe { UserspaceMutPtr::try_from_usize(path)? };
    sys_getcwd(&cx, path, size)
}

fn dispatch_sys_mmap(
    addr: usize,
    len: usize,
    prot: usize,
    flags: usize,
    fd: usize,
    offset: usize,
) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let addr = unsafe { UserspacePtr::try_from_usize(addr)? };
    let prot = i32::try_from(prot)?;
    let flags = i32::try_from(flags)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(i32::try_from(fd)?);
    sys_mmap(&cx, addr, len, prot, flags, fd, offset)
}

fn dispatch_sys_open(
    path: usize,
    path_len: usize,
    oflag: usize,
    mode: usize,
) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let path = unsafe { UserspacePtr::try_from_usize(path)? };
    sys_open(&cx, path, path_len, oflag as i32, mode as i32)
}

fn dispatch_sys_read(fd: usize, buf: usize, nbyte: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let slice = unsafe { slice_from_ptr_and_len_mut(buf, nbyte) }?;
    sys_read(&cx, fd, slice)
}

fn dispatch_sys_write(fd: usize, buf: usize, nbyte: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let slice = unsafe { slice_from_ptr_and_len(buf, nbyte) }?;
    sys_write(&cx, fd, slice)
}
