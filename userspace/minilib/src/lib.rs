#![no_std]

extern crate alloc;

mod backtrace;
mod heap;
mod panic;

use core::arch::asm;
use core::arch::x86_64::_mm_pause;
use core::ffi::c_int;

pub use kernel_abi::{
    CLOCK_MONOTONIC, DefaultAction, EFAULT, EINVAL, ENOTTY, ESRCH, Errno, FbScreenInfo,
    IoctlRequest, MapFlags, ProtFlags, SYS_CLOCK_GETTIME, SYS_EXE_PATH, SYS_EXIT, SYS_FSTAT,
    SYS_FSYNC, SYS_GETPID, SYS_IOCTL, SYS_KILL, SYS_LSEEK, SYS_MMAP, SYS_OPEN, SYS_READ,
    SYS_SIGACTION, SYS_SIGPENDING, SYS_SIGPROCMASK, SYS_SIGRETURN, SYS_WRITE, SaFlags, SigAction,
    SigHandler, SigMaskHow, SigSet, Signal, Stat, Timespec, Whence,
};
pub use panic::catch_unwind;

pub fn exit(code: i32) -> ! {
    syscall1(SYS_EXIT, code as usize);
    loop {
        _mm_pause();
    }
}

pub fn read(fd: c_int, buf: &mut [u8]) -> c_int {
    syscall3(SYS_READ, fd as usize, buf.as_mut_ptr() as usize, buf.len()) as i32
}

pub fn write(fd: c_int, buf: &[u8]) -> c_int {
    syscall3(SYS_WRITE, fd as usize, buf.as_ptr() as usize, buf.len()) as i32
}

pub fn lseek(fd: c_int, offset: i64, whence: Whence) -> i64 {
    syscall3(SYS_LSEEK, fd as usize, offset as usize, whence as usize) as i64
}

pub fn fstat(fd: c_int, stat: &mut Stat) -> c_int {
    syscall2(SYS_FSTAT, fd as usize, core::ptr::from_mut(stat) as usize) as c_int
}

pub fn ioctl<T>(fd: c_int, request: IoctlRequest, arg: &mut T) -> c_int {
    syscall3(
        SYS_IOCTL,
        fd as usize,
        request.number(),
        core::ptr::from_mut(arg) as usize,
    ) as c_int
}

pub fn fsync(fd: c_int) -> c_int {
    syscall1(SYS_FSYNC, fd as usize) as c_int
}

pub fn clock_gettime(clockid: usize, tp: &mut Timespec) -> c_int {
    syscall2(SYS_CLOCK_GETTIME, clockid, tp as *mut Timespec as usize) as c_int
}

pub fn exe_path(buf: &mut [u8]) -> isize {
    syscall2(SYS_EXE_PATH, buf.as_mut_ptr() as usize, buf.len()) as isize
}

#[unsafe(naked)]
pub extern "C" fn sigreturn_restorer() {
    core::arch::naked_asm!("mov rax, {n}", "int 0x80", n = const SYS_SIGRETURN)
}

pub fn kill(pid: i64, signo: Signal) -> c_int {
    syscall2(SYS_KILL, pid as usize, signo.number() as usize) as c_int
}

pub fn sigaction(signo: Signal, new: Option<&SigAction>, old: Option<&mut SigAction>) -> c_int {
    let new_ptr = new.map_or(0, |a| a as *const SigAction as usize);
    let old_ptr = old.map_or(0, |a| a as *mut SigAction as usize);
    syscall3(SYS_SIGACTION, signo.number() as usize, new_ptr, old_ptr) as c_int
}

pub fn sigprocmask(how: SigMaskHow, set: Option<&SigSet>, old: Option<&mut SigSet>) -> c_int {
    let set_ptr = set.map_or(0, |s| s as *const SigSet as usize);
    let old_ptr = old.map_or(0, |s| s as *mut SigSet as usize);
    syscall3(SYS_SIGPROCMASK, how as usize, set_ptr, old_ptr) as c_int
}

pub fn sigpending(out: &mut SigSet) -> c_int {
    syscall1(SYS_SIGPENDING, out as *mut SigSet as usize) as c_int
}

pub fn getpid() -> i64 {
    syscall0(SYS_GETPID) as i64
}

pub fn install_handler(signo: Signal, handler: extern "C" fn(Signal)) -> c_int {
    let action = SigAction {
        handler: SigHandler::new(handler as usize),
        mask: 0,
        flags: SaFlags::default(),
        restorer: sigreturn_restorer as *const () as usize,
    };
    sigaction(signo, Some(&action), None)
}

pub fn syscall0(n: usize) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            options(nostack),
        );
    }
    result
}

pub fn syscall1(n: usize, arg1: usize) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            options(nostack),
        );
    }
    result
}

pub fn syscall2(n: usize, arg1: usize, arg2: usize) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            in("rsi") arg2,
            options(nostack),
        );
    }
    result
}

pub fn syscall3(n: usize, arg1: usize, arg2: usize, arg3: usize) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            options(nostack),
        );
    }
    result
}

pub fn syscall6(
    n: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("rcx") arg4,
            in("r8") arg5,
            in("r9") arg6,
            options(nostack),
        );
    }
    result
}

pub fn mmap(
    addr: usize,
    len: usize,
    prot: ProtFlags,
    flags: MapFlags,
    fd: usize,
    offset: usize,
) -> isize {
    syscall6(
        SYS_MMAP,
        addr,
        len,
        prot.bits() as usize,
        flags.bits() as usize,
        fd,
        offset,
    ) as isize
}

pub fn open(path: &str) -> c_int {
    syscall6(SYS_OPEN, path.as_ptr() as usize, path.len(), 0, 0, 0, 0) as c_int
}
