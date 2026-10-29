#![no_std]

extern crate alloc;

mod backtrace;
mod heap;
mod io;
mod panic;
mod start;

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::arch::x86_64::_mm_pause;
use core::ffi::c_int;

pub use io::{Stderr, Stdout};
pub use kernel_abi::{
    ARG_MAX, ClockId, DefaultAction, DirentHeader, E2BIG, EACCES, EBADF, EEXIST, EFAULT, EINTR,
    EINVAL, EISDIR, ENAMETOOLONG, ENOENT, ENOEXEC, ENOMEM, ENOSYS, ENOTDIR, ENOTTY, EOVERFLOW,
    EPERM, ERANGE, ESPIPE, ESRCH, Errno, FbScreenInfo, IoctlRequest, MapFlags, O_ACCMODE, O_APPEND,
    O_CLOEXEC, O_CLOFORK, O_CREAT, O_DIRECTORY, O_DSYNC, O_EXCL, O_EXEC, O_NOCTTY, O_NOFOLLOW,
    O_NONBLOCK, O_RDONLY, O_RDWR, O_RSYNC, O_SEARCH, O_SYNC, O_TRUNC, O_TTY_INIT, O_WRONLY,
    PATH_MAX, ProtFlags, SYS_CLOCK_GETTIME, SYS_CLOSE, SYS_EXE_PATH, SYS_EXECVE, SYS_EXIT,
    SYS_FORK, SYS_FSTAT, SYS_FSYNC, SYS_GETCWD, SYS_GETDENTS, SYS_GETPID, SYS_GETPPID, SYS_IOCTL,
    SYS_KILL, SYS_LSEEK, SYS_MMAP, SYS_NANOSLEEP, SYS_OPEN, SYS_READ, SYS_SHUTDOWN, SYS_SIGACTION,
    SYS_SIGPENDING, SYS_SIGPROCMASK, SYS_SIGRETURN, SYS_WAITPID, SYS_WRITE, SaFlags, SigAction,
    SigHandler, SigMaskHow, SigSet, Signal, Stat, StrSlice, Timespec, Whence,
};
pub use panic::catch_unwind;
pub use start::{__muffin_start_inner, args, env};

pub fn exit(code: i32) -> ! {
    unsafe { syscall1(SYS_EXIT, code as usize) };
    loop {
        _mm_pause();
    }
}

pub fn shutdown() -> ! {
    unsafe { syscall0(SYS_SHUTDOWN) };
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
    // Safety: `buf` is a live, exclusive slice for the whole call.
    ret(unsafe { syscall3(SYS_READ, fd as usize, buf.as_mut_ptr() as usize, buf.len()) })
}

pub fn write(fd: c_int, buf: &[u8]) -> Result<usize, Errno> {
    // Safety: `buf` is a live slice for the whole call.
    ret(unsafe { syscall3(SYS_WRITE, fd as usize, buf.as_ptr() as usize, buf.len()) })
}

/// Fills `buf` with whole directory records and returns the number of bytes
/// written. `Ok(0)` means no entries remain.
///
/// # Errors
/// Returns the kernel's errno, such as `ENOTDIR` when `fd` is not a directory
/// or `EINVAL` when `buf` cannot hold the next record.
pub fn getdents(fd: c_int, buf: &mut [u8]) -> Result<usize, Errno> {
    // Safety: `buf` is a live, exclusive slice for the whole call.
    ret(unsafe {
        syscall3(
            SYS_GETDENTS,
            fd as usize,
            buf.as_mut_ptr() as usize,
            buf.len(),
        )
    })
}

/// Returns the names in the directory at `path`, including `.` and `..`, in
/// the order the file system stores them.
///
/// # Errors
/// Returns the errno of `open` or `getdents`, such as `ENOENT` or `ENOTDIR`.
pub fn read_dir(path: &str) -> Result<Vec<String>, Errno> {
    let fd = open(path, O_RDONLY)?;
    let mut names = Vec::new();
    let result = read_dir_fd(fd, &mut names);
    let closed = close(fd);
    result.and(closed).map(|()| names)
}

fn read_dir_fd(fd: c_int, names: &mut Vec<String>) -> Result<(), Errno> {
    let mut buf = [0_u8; 1024];
    loop {
        let len = getdents(fd, &mut buf)?;
        if len == 0 {
            return Ok(());
        }
        let mut records = &buf[..len];
        while !records.is_empty() {
            let header = DirentHeader {
                name_len: records[0],
            };
            let body = &records[size_of::<DirentHeader>()..];
            let (name, rest) = body.split_at(header.name_len.into());
            // The kernel only lists names that are valid UTF-8.
            names.push(String::from_utf8_lossy(name).into_owned());
            records = rest;
        }
    }
}

pub fn lseek(fd: c_int, offset: i64, whence: Whence) -> Result<u64, Errno> {
    ret(unsafe { syscall3(SYS_LSEEK, fd as usize, offset as usize, whence as usize) })
        .map(|off| off as u64)
}

pub fn fstat(fd: c_int, stat: &mut Stat) -> Result<(), Errno> {
    // Safety: `stat` is a live, exclusive reference for the whole call.
    ret(unsafe { syscall2(SYS_FSTAT, fd as usize, core::ptr::from_mut(stat) as usize) }).map(|_| ())
}

pub fn ioctl<T>(fd: c_int, request: IoctlRequest, arg: &mut T) -> Result<usize, Errno> {
    // Safety: `arg` is a live, exclusive reference for the whole call.
    ret(unsafe {
        syscall3(
            SYS_IOCTL,
            fd as usize,
            request.number(),
            core::ptr::from_mut(arg) as usize,
        )
    })
}

pub fn fsync(fd: c_int) -> Result<(), Errno> {
    ret(unsafe { syscall1(SYS_FSYNC, fd as usize) }).map(|_| ())
}

pub fn close(fd: c_int) -> Result<(), Errno> {
    ret(unsafe { syscall1(SYS_CLOSE, fd as usize) }).map(|_| ())
}

pub fn clock_gettime(clockid: ClockId, tp: &mut Timespec) -> Result<(), Errno> {
    // Safety: `tp` is a live, exclusive reference for the whole call.
    ret(unsafe {
        syscall2(
            SYS_CLOCK_GETTIME,
            clockid as usize,
            tp as *mut Timespec as usize,
        )
    })
    .map(|_| ())
}

pub fn nanosleep(req: &Timespec, rem: Option<&mut Timespec>) -> Result<(), Errno> {
    let rem_ptr = rem.map_or(0, |r| r as *mut Timespec as usize);
    // Safety: `req` and `rem` are live references for the whole call, `rem`
    // exclusive.
    ret(unsafe { syscall2(SYS_NANOSLEEP, req as *const Timespec as usize, rem_ptr) }).map(|_| ())
}

/// Returns the number of path bytes written into `buf`.
pub fn exe_path(buf: &mut [u8]) -> Result<usize, Errno> {
    // Safety: `buf` is a live, exclusive slice for the whole call.
    ret(unsafe { syscall2(SYS_EXE_PATH, buf.as_mut_ptr() as usize, buf.len()) })
}

#[unsafe(naked)]
pub extern "C" fn sigreturn_restorer() {
    core::arch::naked_asm!("mov rax, {n}", "int 0x80", n = const SYS_SIGRETURN)
}

pub fn kill(pid: i64, signo: Signal) -> Result<(), Errno> {
    ret(unsafe { syscall2(SYS_KILL, pid as usize, signo.number() as usize) }).map(|_| ())
}

pub fn sigaction(
    signo: Signal,
    new: Option<&SigAction>,
    old: Option<&mut SigAction>,
) -> Result<(), Errno> {
    let new_ptr = new.map_or(0, |a| a as *const SigAction as usize);
    let old_ptr = old.map_or(0, |a| a as *mut SigAction as usize);
    // Safety: `new` and `old` are live references for the whole call, `old`
    // exclusive.
    ret(unsafe { syscall3(SYS_SIGACTION, signo.number() as usize, new_ptr, old_ptr) }).map(|_| ())
}

pub fn sigprocmask(
    how: SigMaskHow,
    set: Option<&SigSet>,
    old: Option<&mut SigSet>,
) -> Result<(), Errno> {
    let set_ptr = set.map_or(0, |s| s as *const SigSet as usize);
    let old_ptr = old.map_or(0, |s| s as *mut SigSet as usize);
    // Safety: `set` and `old` are live references for the whole call, `old`
    // exclusive.
    ret(unsafe { syscall3(SYS_SIGPROCMASK, how as usize, set_ptr, old_ptr) }).map(|_| ())
}

pub fn sigpending(out: &mut SigSet) -> Result<(), Errno> {
    // Safety: `out` is a live, exclusive reference for the whole call.
    ret(unsafe { syscall1(SYS_SIGPENDING, out as *mut SigSet as usize) }).map(|_| ())
}

pub fn getpid() -> i64 {
    unsafe { syscall0(SYS_GETPID) as i64 }
}

/// Returns `Ok(0)` in the child and `Ok(child_pid)` in the parent.
pub fn fork() -> Result<i64, Errno> {
    ret(unsafe { syscall0(SYS_FORK) }).map(|pid| pid as i64)
}

pub fn getppid() -> i64 {
    unsafe { syscall0(SYS_GETPPID) as i64 }
}

/// Waits until the child `pid` exits and returns its status: the exit code,
/// or 128 plus the signal number.
pub fn waitpid(pid: i64) -> Result<i32, Errno> {
    let mut status = 0i32;
    ret(unsafe { syscall2(SYS_WAITPID, pid as usize, &raw mut status as usize) })?;
    Ok(status)
}

/// Builds the action that runs `handler` with an empty mask and returns
/// through [`sigreturn_restorer`].
pub fn handler_action(handler: extern "C" fn(Signal), flags: SaFlags) -> SigAction {
    SigAction {
        handler: SigHandler::new(handler as usize),
        mask: SigSet::EMPTY,
        flags,
        restorer: sigreturn_restorer as *const () as usize,
    }
}

pub fn install_handler(signo: Signal, handler: extern "C" fn(Signal)) -> Result<(), Errno> {
    sigaction(
        signo,
        Some(&handler_action(handler, SaFlags::default())),
        None,
    )
}

/// Issues raw syscall `n`.
///
/// # Safety
///
/// The kernel reads and writes memory through whichever arguments syscall `n`
/// takes as pointers. The caller must keep every such pointer valid, correctly
/// typed and unaliased for the duration of the call.
pub unsafe fn syscall0(n: usize) -> usize {
    let result;
    // Safety: the kernel preserves every register except rax across int 0x80,
    // and the memory it touches is the caller's obligation.
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            options(nostack),
        );
    }
    result
}

/// Issues raw syscall `n`.
///
/// # Safety
///
/// See [`syscall0`].
pub unsafe fn syscall1(n: usize, arg1: usize) -> usize {
    let result;
    // Safety: see `syscall0`.
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            options(nostack),
        );
    }
    result
}

/// Issues raw syscall `n`.
///
/// # Safety
///
/// See [`syscall0`].
pub unsafe fn syscall2(n: usize, arg1: usize, arg2: usize) -> usize {
    let result;
    // Safety: see `syscall0`.
    unsafe {
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

/// Issues raw syscall `n`.
///
/// # Safety
///
/// See [`syscall0`].
pub unsafe fn syscall3(n: usize, arg1: usize, arg2: usize, arg3: usize) -> usize {
    let result;
    // Safety: see `syscall0`.
    unsafe {
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

/// Issues raw syscall `n`.
///
/// # Safety
///
/// See [`syscall0`].
pub unsafe fn syscall6(
    n: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> usize {
    let result;
    // Safety: see `syscall0`.
    unsafe {
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
    fd: c_int,
    offset: usize,
) -> Result<*mut u8, Errno> {
    ret(unsafe {
        syscall6(
            SYS_MMAP,
            addr,
            len,
            prot.bits() as usize,
            flags.bits() as usize,
            fd as usize,
            offset,
        )
    })
    .map(|a| a as *mut u8)
}

/// Opens `path` with the `O_*` flags in `oflag` and returns the new fd.
///
/// # Errors
/// Returns the kernel's errno, such as `ENOENT` for a missing file without
/// `O_CREAT` or `EINVAL` for an invalid `oflag`.
pub fn open(path: &str, oflag: c_int) -> Result<c_int, Errno> {
    // Safety: `path` is a live slice for the whole call.
    ret(unsafe {
        syscall6(
            SYS_OPEN,
            path.as_ptr() as usize,
            path.len(),
            oflag as usize,
            0,
            0,
            0,
        )
    })
    .map(|fd| fd as c_int)
}

/// Never returns on success, so the result is always the failure reason.
pub fn execve(path: &str, argv: &[&str], envp: &[&str]) -> Errno {
    let argv_v = argv.iter().map(|&s| StrSlice::from(s)).collect::<Vec<_>>();
    let envp_v = envp.iter().map(|&s| StrSlice::from(s)).collect::<Vec<_>>();
    // Safety: `path`, `argv_v` and `envp_v` are live for the whole call.
    let raw = unsafe {
        syscall6(
            SYS_EXECVE,
            path.as_ptr() as usize,
            path.len(),
            argv_v.as_ptr() as usize,
            argv_v.len(),
            envp_v.as_ptr() as usize,
            envp_v.len(),
        )
    };
    Errno::from(-(raw as isize))
}
