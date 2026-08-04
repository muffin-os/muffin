use core::ops::Neg;
use core::slice::{from_raw_parts, from_raw_parts_mut};

use access::KernelAccess;
use kernel_abi::{
    EFAULT, EINVAL, EIO, ENOMEM, ESRCH, Errno, IoctlRequest, ProcessId, SigAction, SigMaskHow,
    SigSet, Signal, Timespec, syscall_name,
};
use kernel_syscall::access::{FileAccess, ProcessesAccess};
use kernel_syscall::fcntl::sys_open;
use kernel_syscall::mman::sys_mmap;
use kernel_syscall::signal::{SignalTarget, sys_kill};
use kernel_syscall::unistd::{sys_fsync, sys_getcwd, sys_ioctl, sys_read, sys_write};
use kernel_syscall::{UserspaceMutPtr, UserspacePtr};
use tracing::{debug, error};
use x86_64::VirtAddr;
use x86_64::instructions::hlt;

use crate::hpet::hpet;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::ExitOutcome;
use crate::mcore::mtask::process::mem::PageInError;
use crate::mcore::mtask::task::Task;

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
    let result = match n {
        kernel_abi::SYS_GETCWD => dispatch_sys_getcwd(arg1, arg2),
        kernel_abi::SYS_MMAP => dispatch_sys_mmap(arg1, arg2, arg3, arg4, arg5, arg6),
        kernel_abi::SYS_OPEN => dispatch_sys_open(arg1, arg2, arg3, arg4),
        kernel_abi::SYS_READ => dispatch_sys_read(arg1, arg2, arg3),
        kernel_abi::SYS_WRITE => dispatch_sys_write(arg1, arg2, arg3),
        kernel_abi::SYS_EXIT => dispatch_sys_exit(arg1),
        kernel_abi::SYS_GETPID => dispatch_sys_getpid(),
        kernel_abi::SYS_KILL => dispatch_sys_kill(arg1, arg2),
        kernel_abi::SYS_SIGACTION => dispatch_sys_sigaction(arg1, arg2, arg3),
        kernel_abi::SYS_SIGPROCMASK => dispatch_sys_sigprocmask(arg1, arg2, arg3),
        kernel_abi::SYS_SIGPENDING => dispatch_sys_sigpending(arg1),
        kernel_abi::SYS_IOCTL => dispatch_sys_ioctl(arg1, arg2, arg3),
        kernel_abi::SYS_FSYNC => dispatch_sys_fsync(arg1),
        kernel_abi::SYS_CLOCK_GETTIME => dispatch_sys_clock_gettime(arg1, arg2),
        _ => {
            error!("unimplemented syscall: {} ({n})", syscall_name(n));
            loop {
                hlt();
            }
        }
    };

    match result {
        Ok(ret) => ret as isize,
        Err(e) => {
            error!("syscall {} ({n}) failed with error: {e:?}", syscall_name(n));
            Into::<isize>::into(e).neg()
        }
    }
}

fn dispatch_sys_exit(code: usize) -> Result<usize, Errno> {
    let ctx = ExecutionContext::load();
    debug!("process {} exit with code {code}", ctx.pid());
    ctx.current_process()
        .set_exit_outcome(ExitOutcome::Exited(code));
    Task::exit();
    // Task::exit never returns, it parks the task until the scheduler reaps it.
    Ok(0)
}

fn dispatch_sys_getpid() -> Result<usize, Errno> {
    Ok(ExecutionContext::load().pid().as_u64() as usize)
}

fn dispatch_sys_kill(pid: usize, signo: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    // POSIX pid encoding: > 0 targets that process, 0 the caller's process
    // group (sys_kill substitutes the caller's pgid for the root id), -1
    // broadcasts, < -1 targets the group -pid.
    let target = match pid as isize {
        1.. => SignalTarget::SpecificProcess(ProcessId::from(pid as u64)),
        0 => SignalTarget::ProcessGroup(ProcessId::from(0_u64)),
        -1 => SignalTarget::BroadcastAll,
        v => SignalTarget::ProcessGroup(ProcessId::from(v.unsigned_abs() as u64)),
    };

    if signo as i32 == 0 {
        // POSIX existence probe: no signal is sent, only the target is resolved.
        let exists = match target {
            SignalTarget::SpecificProcess(pid) => cx.process_by_id(pid).is_some(),
            SignalTarget::ProcessGroup(pgid) => {
                let effective = if pgid.is_root() {
                    ExecutionContext::load().pid()
                } else {
                    pgid
                };
                cx.processes_in_group(effective).next().is_some()
            }
            SignalTarget::BroadcastAll => true,
        };
        return if exists { Ok(0) } else { Err(ESRCH) };
    }

    let signal = Signal::try_from(signo as i32)?;
    sys_kill(&cx, target, signal)
}

/// Copies `value` to the userspace pointer `addr`, returning `EFAULT` if the
/// destination is not a mapped, writable, user page. Validating up front keeps
/// a bad pointer from faulting the copy-out in Ring 0 and panicking the kernel.
fn write_user<T>(addr: usize, value: T) -> Result<(), Errno> {
    let mut ptr = unsafe { UserspaceMutPtr::<T>::try_from_usize(addr)? };
    ptr.validate_range(size_of::<T>())?;

    let Ok(vaddr) = VirtAddr::try_new(addr as u64) else {
        return Err(EFAULT);
    };
    if !ExecutionContext::load()
        .current_process()
        .address_space()
        .is_user_writable(vaddr, size_of::<T>())
    {
        return Err(EFAULT);
    }
    unsafe {
        // Safety: the range is lower-half, mapped, writable, and user
        // accessible, so this write cannot fault.
        ptr.as_mut_ptr().write_unaligned(value);
    }
    Ok(())
}

fn dispatch_sys_sigaction(signo: usize, new: usize, old: usize) -> Result<usize, Errno> {
    let signal = Signal::try_from(signo as i32)?;

    let new_action = if new == 0 {
        None
    } else {
        let ptr = unsafe { UserspacePtr::<SigAction>::try_from_usize(new)? };
        ptr.validate_range(size_of::<SigAction>())?;
        // Safety: range-validated lower-half pointer, read by value
        Some(unsafe { ptr.as_ptr().read_unaligned() })
    };

    let process = ExecutionContext::load().current_process();
    let old_action = process.signals().write().sigaction(signal, new_action)?;

    if old != 0 {
        write_user(old, old_action)?;
    }
    Ok(0)
}

fn dispatch_sys_sigprocmask(how: usize, set: usize, oldset: usize) -> Result<usize, Errno> {
    let how = SigMaskHow::try_from(how)?;

    let new_set = if set == 0 {
        None
    } else {
        let ptr = unsafe { UserspacePtr::<SigSet>::try_from_usize(set)? };
        ptr.validate_range(size_of::<SigSet>())?;
        // Safety: range-validated lower-half pointer, read by value
        Some(unsafe { ptr.as_ptr().read_unaligned() })
    };

    let process = ExecutionContext::load().current_process();
    let old_mask = process.signals().write().sigprocmask(how, new_set)?;

    if oldset != 0 {
        write_user(oldset, old_mask)?;
    }
    Ok(0)
}

fn dispatch_sys_sigpending(out: usize) -> Result<usize, Errno> {
    if out == 0 {
        return Err(EINVAL);
    }
    let pending = ExecutionContext::load()
        .current_process()
        .signals()
        .read()
        .sigpending();
    write_user(out, pending)?;
    Ok(0)
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

enum UserAccess {
    Read,
    Write,
}

/// Makes `[ptr, ptr + len)` resident, then checks it against `access`.
///
/// Every syscall that goes on to take a filesystem lock must call this first.
/// The page fault handler pages in file backed memory through that same per
/// mount lock, so a fault raised while the lock is held re-enters a
/// non-reentrant lock and hangs the CPU.
///
/// A page that no memory region backs is left unmapped and reported, never
/// paged in.
///
/// # Errors
/// `EFAULT` for a non-canonical address, for a page that cannot be mapped, and
/// for a range that is resident but not reachable with `access`. `ENOMEM` when
/// no physical frame is available. `EIO` when the backing file read fails.
fn make_user_range_resident(ptr: usize, len: usize, access: UserAccess) -> Result<(), Errno> {
    let Ok(addr) = VirtAddr::try_new(ptr as u64) else {
        return Err(EFAULT);
    };

    let process = ExecutionContext::load().current_process();
    let address_space = process.address_space();
    process
        .memory_regions()
        .populate(address_space, addr, len)
        .map_err(|e| match e {
            PageInError::OutOfMemory => ENOMEM,
            PageInError::MapFailed => EFAULT,
            PageInError::ReadFailed => EIO,
        })?;

    let accessible = match access {
        UserAccess::Read => address_space.is_user_readable(addr, len),
        UserAccess::Write => address_space.is_user_writable(addr, len),
    };
    if accessible { Ok(()) } else { Err(EFAULT) }
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
    let fd = i32::try_from(fd)?;
    sys_mmap(&cx, addr, len, prot, flags, fd, offset)
}

fn dispatch_sys_open(
    path: usize,
    path_len: usize,
    oflag: usize,
    mode: usize,
) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    make_user_range_resident(path, path_len, UserAccess::Read)?;
    let path = unsafe { UserspacePtr::try_from_usize(path)? };
    sys_open(&cx, path, path_len, oflag as i32, mode as i32)
}

fn dispatch_sys_read(fd: usize, buf: usize, nbyte: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let slice = unsafe { slice_from_ptr_and_len_mut(buf, nbyte) }?;
    make_user_range_resident(buf, nbyte, UserAccess::Write)?;
    sys_read(&cx, fd, slice)
}

fn dispatch_sys_write(fd: usize, buf: usize, nbyte: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let slice = unsafe { slice_from_ptr_and_len(buf, nbyte) }?;
    make_user_range_resident(buf, nbyte, UserAccess::Read)?;
    sys_write(&cx, fd, slice)
}

fn dispatch_sys_ioctl(fd: usize, request: usize, argp: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let request = IoctlRequest::try_from(request)?;
    match request.arg_size() {
        0 => sys_ioctl(&cx, fd, request, &mut []),
        size => {
            let arg = unsafe { slice_from_ptr_and_len_mut(argp, size) }?;
            make_user_range_resident(argp, size, UserAccess::Write)?;
            sys_ioctl(&cx, fd, request, arg)
        }
    }
}

fn dispatch_sys_fsync(fd: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    sys_fsync(&cx, fd)
}

/// POSIX clock_gettime backed by the HPET main counter
///
/// hpet() panics only before HPET init, but syscalls only run long after init, so it cannot panic here
fn dispatch_sys_clock_gettime(clockid: usize, tp: usize) -> Result<usize, Errno> {
    // elapsed_ns converts main counter ticks to ns since boot using the HPET period
    let ns = u128::from(hpet().read().elapsed_ns());

    let secs_since_boot = ns / 1_000_000_000;
    // tv_nsec is always below 1e9 so the cast cannot truncate
    let tv_nsec = (ns % 1_000_000_000) as i64;

    let total_secs = match clockid {
        kernel_abi::CLOCK_MONOTONIC => secs_since_boot,
        kernel_abi::CLOCK_REALTIME => {
            let boot = *crate::BOOT_TIME_SECONDS.get().ok_or(EINVAL)?;
            boot as u128 + secs_since_boot
        }
        _ => return Err(EINVAL),
    };

    // seconds since epoch stay within i64 for centuries, but convert fallibly to avoid truncation
    let tv_sec = i64::try_from(total_secs).map_err(|_| EINVAL)?;
    write_user::<Timespec>(tp, Timespec { tv_sec, tv_nsec })?;
    Ok(0)
}
