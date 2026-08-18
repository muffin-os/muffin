#![no_std]

extern crate alloc;

mod backtrace;
mod heap;
mod io;
mod panic;
mod start;

use alloc::vec::Vec;
use core::arch::asm;
use core::arch::x86_64::_mm_pause;
use core::ffi::c_int;

pub use io::{Stderr, Stdout};
pub use kernel_abi::{
    ARG_MAX, CLOCK_MONOTONIC, CLOCK_REALTIME, DefaultAction, E2BIG, EACCES, EBADF, EFAULT, EINTR,
    EINVAL, EISDIR, ENAMETOOLONG, ENOENT, ENOEXEC, ENOMEM, ENOTDIR, ENOTTY, EOVERFLOW, EPERM,
    ERANGE, ESPIPE, ESRCH, Errno, FbScreenInfo, IoctlRequest, MapFlags, PATH_MAX, ProtFlags,
    SYS_CLOCK_GETTIME, SYS_EXE_PATH, SYS_EXECVE, SYS_EXIT, SYS_FSTAT, SYS_FSYNC, SYS_GETCWD,
    SYS_GETPID, SYS_IOCTL, SYS_KILL, SYS_LSEEK, SYS_MMAP, SYS_NANOSLEEP, SYS_OPEN, SYS_READ,
    SYS_SIGACTION, SYS_SIGPENDING, SYS_SIGPROCMASK, SYS_SIGRETURN, SYS_WRITE, SaFlags, SigAction,
    SigHandler, SigMaskHow, SigSet, Signal, Stat, StrSlice, Timespec, Whence,
};
pub use panic::catch_unwind;
pub use start::{__muffin_start_inner, args, env};

pub fn exit(code: i32) -> ! {
    syscall1(SYS_EXIT, code as usize);
    loop {
        _mm_pause();
    }
}

/// Splits the kernel's raw return register into a value or a negated errno.
pub fn ret(raw: usize) -> Result<usize, Errno> {
    let v = raw as isize;
    if v < 0 { Err(Errno::from(-v)) } else { Ok(raw) }
}

pub fn read(fd: c_int, buf: &mut [u8]) -> Result<usize, Errno> {
    ret(syscall3(
        SYS_READ,
        fd as usize,
        buf.as_mut_ptr() as usize,
        buf.len(),
    ))
}

pub fn write(fd: c_int, buf: &[u8]) -> Result<usize, Errno> {
    ret(syscall3(
        SYS_WRITE,
        fd as usize,
        buf.as_ptr() as usize,
        buf.len(),
    ))
}

pub fn lseek(fd: c_int, offset: i64, whence: Whence) -> Result<u64, Errno> {
    ret(syscall3(
        SYS_LSEEK,
        fd as usize,
        offset as usize,
        whence as usize,
    ))
    .map(|off| off as u64)
}

pub fn fstat(fd: c_int, stat: &mut Stat) -> Result<(), Errno> {
    ret(syscall2(
        SYS_FSTAT,
        fd as usize,
        core::ptr::from_mut(stat) as usize,
    ))
    .map(|_| ())
}

pub fn ioctl<T>(fd: c_int, request: IoctlRequest, arg: &mut T) -> Result<usize, Errno> {
    ret(syscall3(
        SYS_IOCTL,
        fd as usize,
        request.number(),
        core::ptr::from_mut(arg) as usize,
    ))
}

pub fn fsync(fd: c_int) -> Result<(), Errno> {
    ret(syscall1(SYS_FSYNC, fd as usize)).map(|_| ())
}

pub fn clock_gettime(clockid: usize, tp: &mut Timespec) -> Result<(), Errno> {
    ret(syscall2(
        SYS_CLOCK_GETTIME,
        clockid,
        tp as *mut Timespec as usize,
    ))
    .map(|_| ())
}

pub fn nanosleep(req: &Timespec, rem: Option<&mut Timespec>) -> Result<(), Errno> {
    let rem_ptr = rem.map_or(0, |r| r as *mut Timespec as usize);
    ret(syscall2(
        SYS_NANOSLEEP,
        req as *const Timespec as usize,
        rem_ptr,
    ))
    .map(|_| ())
}

/// Returns the number of path bytes written into `buf`.
pub fn exe_path(buf: &mut [u8]) -> Result<usize, Errno> {
    ret(syscall2(SYS_EXE_PATH, buf.as_mut_ptr() as usize, buf.len()))
}

#[unsafe(naked)]
pub extern "C" fn sigreturn_restorer() {
    core::arch::naked_asm!("mov rax, {n}", "int 0x80", n = const SYS_SIGRETURN)
}

pub fn kill(pid: i64, signo: Signal) -> Result<(), Errno> {
    ret(syscall2(SYS_KILL, pid as usize, signo.number() as usize)).map(|_| ())
}

pub fn sigaction(
    signo: Signal,
    new: Option<&SigAction>,
    old: Option<&mut SigAction>,
) -> Result<(), Errno> {
    let new_ptr = new.map_or(0, |a| a as *const SigAction as usize);
    let old_ptr = old.map_or(0, |a| a as *mut SigAction as usize);
    ret(syscall3(
        SYS_SIGACTION,
        signo.number() as usize,
        new_ptr,
        old_ptr,
    ))
    .map(|_| ())
}

pub fn sigprocmask(
    how: SigMaskHow,
    set: Option<&SigSet>,
    old: Option<&mut SigSet>,
) -> Result<(), Errno> {
    let set_ptr = set.map_or(0, |s| s as *const SigSet as usize);
    let old_ptr = old.map_or(0, |s| s as *mut SigSet as usize);
    ret(syscall3(SYS_SIGPROCMASK, how as usize, set_ptr, old_ptr)).map(|_| ())
}

pub fn sigpending(out: &mut SigSet) -> Result<(), Errno> {
    ret(syscall1(SYS_SIGPENDING, out as *mut SigSet as usize)).map(|_| ())
}

pub fn getpid() -> i64 {
    syscall0(SYS_GETPID) as i64
}

pub fn install_handler(signo: Signal, handler: extern "C" fn(Signal)) -> Result<(), Errno> {
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
) -> Result<*mut u8, Errno> {
    ret(syscall6(
        SYS_MMAP,
        addr,
        len,
        prot.bits() as usize,
        flags.bits() as usize,
        fd,
        offset,
    ))
    .map(|a| a as *mut u8)
}

pub fn open(path: &str) -> Result<c_int, Errno> {
    ret(syscall6(
        SYS_OPEN,
        path.as_ptr() as usize,
        path.len(),
        0,
        0,
        0,
        0,
    ))
    .map(|fd| fd as c_int)
}

/// Never returns on success, so the result is always the failure reason.
pub fn execve(path: &str, argv: &[&str], envp: &[&str]) -> Errno {
    let argv_v = argv.iter().map(|&s| StrSlice::from(s)).collect::<Vec<_>>();
    let envp_v = envp.iter().map(|&s| StrSlice::from(s)).collect::<Vec<_>>();
    let raw = syscall6(
        SYS_EXECVE,
        path.as_ptr() as usize,
        path.len(),
        argv_v.as_ptr() as usize,
        argv_v.len(),
        envp_v.as_ptr() as usize,
        envp_v.len(),
    );
    Errno::from(-(raw as isize))
}
