use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_int;
use core::ops::Neg;
use core::slice::{from_raw_parts, from_raw_parts_mut};
use core::time::Duration;

use access::KernelAccess;
use kernel_abi::{
    AF_UNIX, ClockId, EFAULT, EINTR, EINVAL, EIO, ENOENT, ENOMEM, ENOSYS, ERANGE, ESRCH, Errno,
    IoVec, IoctlRequest, MapFlags, MsgHdr, ProcessId, ProtFlags, SOMAXCONN, SaFamily, ShutdownHow,
    SigAction, SigMaskHow, SigSet, Signal, SockAddrUn, SockLen, Stat, Timespec, WaitOptions,
    Whence, syscall_name,
};
use kernel_syscall::access::{
    FileAccess, MemoryRegionAccess, PermissionAccess, ProcessesAccess, SocketAccess,
};
use kernel_syscall::fcntl::{sys_open, sys_unlink};
use kernel_syscall::mman::{sys_mmap, sys_munmap};
use kernel_syscall::signal::{SignalTarget, sys_kill};
use kernel_syscall::socket::{
    sys_accept, sys_bind, sys_connect, sys_listen, sys_recvmsg, sys_sendmsg, sys_shutdown,
    sys_socket,
};
use kernel_syscall::unistd::{
    sys_close, sys_fsync, sys_ftruncate, sys_getcwd, sys_getdents, sys_ioctl, sys_lseek, sys_read,
    sys_write,
};
use kernel_syscall::wait::{WaitTarget, sys_getppid, sys_waitpid};
use kernel_syscall::{UserspaceMutPtr, UserspacePtr};
use kernel_unix_socket::SOCKET_BUFFER_BYTES;
use tracing::{debug, error};
use x86_64::VirtAddr;
use x86_64::structures::idt::InterruptStackFrame;

use crate::arch::idt::SyscallRegisters;
use crate::hpet::hpet;
use crate::mcore::mtask::process::mem::PageInError;
use crate::mcore::mtask::process::{ExitOutcome, ParkOutcome, Process};
use crate::mcore::mtask::task::Task;

pub(crate) mod access;
mod exec;
mod fork;

#[must_use]
pub(crate) fn dispatch_syscall(
    frame: &mut InterruptStackFrame,
    regs: &mut SyscallRegisters,
) -> isize {
    let n = regs.rax;
    let arg1 = regs.rdi;
    let arg2 = regs.rsi;
    let arg3 = regs.rdx;
    let arg4 = regs.rcx;
    let arg5 = regs.r8;
    let arg6 = regs.r9;

    let result = match n {
        kernel_abi::SYS_GETCWD => dispatch_sys_getcwd(arg1, arg2),
        kernel_abi::SYS_MMAP => dispatch_sys_mmap(arg1, arg2, arg3, arg4, arg5, arg6),
        kernel_abi::SYS_OPEN => dispatch_sys_open(arg1, arg2, arg3, arg4),
        kernel_abi::SYS_READ => dispatch_sys_read(arg1, arg2, arg3),
        kernel_abi::SYS_WRITE => dispatch_sys_write(arg1, arg2, arg3),
        kernel_abi::SYS_EXIT => dispatch_sys_exit(arg1),
        kernel_abi::SYS_GETPID => dispatch_sys_getpid(),
        kernel_abi::SYS_GETPPID => dispatch_sys_getppid(),
        kernel_abi::SYS_WAITPID => dispatch_sys_waitpid(arg1, arg2, arg3),
        kernel_abi::SYS_GETDENTS => dispatch_sys_getdents(arg1, arg2, arg3),
        kernel_abi::SYS_KILL => dispatch_sys_kill(arg1, arg2),
        kernel_abi::SYS_SIGACTION => dispatch_sys_sigaction(arg1, arg2, arg3),
        kernel_abi::SYS_SIGPROCMASK => dispatch_sys_sigprocmask(arg1, arg2, arg3),
        kernel_abi::SYS_SIGPENDING => dispatch_sys_sigpending(arg1),
        kernel_abi::SYS_IOCTL => dispatch_sys_ioctl(arg1, arg2, arg3),
        kernel_abi::SYS_FSYNC => dispatch_sys_fsync(arg1),
        kernel_abi::SYS_CLOSE => dispatch_sys_close(arg1),
        kernel_abi::SYS_FSTAT => dispatch_sys_fstat(arg1, arg2),
        kernel_abi::SYS_LSEEK => dispatch_sys_lseek(arg1, arg2, arg3),
        kernel_abi::SYS_CLOCK_GETTIME => dispatch_sys_clock_gettime(arg1, arg2),
        kernel_abi::SYS_EXE_PATH => dispatch_sys_exe_path(arg1, arg2),
        kernel_abi::SYS_NANOSLEEP => dispatch_sys_nanosleep(arg1, arg2),
        kernel_abi::SYS_EXECVE => {
            exec::dispatch_sys_execve(arg1, arg2, arg3, arg4, arg5, arg6, frame, regs)
        }
        kernel_abi::SYS_FORK => fork::dispatch_sys_fork(frame, regs),
        kernel_abi::SYS_POWEROFF => dispatch_sys_poweroff(),
        kernel_abi::SYS_SOCKET => dispatch_sys_socket(arg1, arg2, arg3),
        kernel_abi::SYS_BIND => dispatch_sys_bind(arg1, arg2, arg3),
        kernel_abi::SYS_LISTEN => dispatch_sys_listen(arg1, arg2),
        kernel_abi::SYS_ACCEPT => dispatch_sys_accept(arg1, arg2, arg3),
        kernel_abi::SYS_CONNECT => dispatch_sys_connect(arg1, arg2, arg3),
        kernel_abi::SYS_SHUTDOWN => dispatch_sys_shutdown(arg1, arg2),
        kernel_abi::SYS_SENDMSG => dispatch_sys_sendmsg(arg1, arg2, arg3),
        kernel_abi::SYS_RECVMSG => dispatch_sys_recvmsg(arg1, arg2, arg3),
        kernel_abi::SYS_FTRUNCATE => dispatch_sys_ftruncate(arg1, arg2),
        kernel_abi::SYS_UNLINK => dispatch_sys_unlink(arg1, arg2),
        kernel_abi::SYS_MUNMAP => dispatch_sys_munmap(arg1, arg2),
        _ => {
            error!("unknown syscall number {n}");
            Err(ENOSYS)
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
    let process = Process::current();
    debug!("process {} exit with code {code}", process.pid());
    process.set_exit_outcome(ExitOutcome::Exited(code));
    Task::exit_current()
}

fn dispatch_sys_poweroff() -> Result<usize, Errno> {
    debug!("process {} requested poweroff", Process::current().pid());
    crate::power::poweroff()
}

fn dispatch_sys_getpid() -> Result<usize, Errno> {
    Ok(Process::current().pid().as_u64() as usize)
}

fn dispatch_sys_getppid() -> Result<usize, Errno> {
    sys_getppid(&KernelAccess::new())
}

fn dispatch_sys_waitpid(pid: usize, status: usize, options: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    // POSIX allows a null status, which skips the status writeback.
    let status = if status == 0 {
        None
    } else {
        let ptr = UserspaceMutPtr::<i32>::try_from_usize(status).map_err(|_| EFAULT)?;
        ptr.validate_range(size_of::<i32>()).map_err(|_| EFAULT)?;
        make_user_range_resident(ptr.addr(), size_of::<i32>(), UserAccess::Write)?;
        Some(ptr)
    };
    let target = match pid as isize {
        1.. => WaitTarget::Child(ProcessId::from(pid as u64)),
        0 => WaitTarget::CallerGroup,
        -1 => WaitTarget::AnyChild,
        v => WaitTarget::ProcessGroup(ProcessId::from(v.unsigned_abs() as u64)),
    };
    let options = WaitOptions::from_bits(i32::try_from(options)?).ok_or(EINVAL)?;
    sys_waitpid(&cx, target, options, status)
}

fn dispatch_sys_kill(pid: usize, signo: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    // POSIX pid encoding: > 0 targets that process, 0 the caller's process
    // group, -1 broadcasts, < -1 targets the group -pid.
    let target = match pid as isize {
        1.. => SignalTarget::SpecificProcess(ProcessId::from(pid as u64)),
        0 => SignalTarget::CallerGroup,
        -1 => SignalTarget::BroadcastAll,
        v => SignalTarget::ProcessGroup(ProcessId::from(v.unsigned_abs() as u64)),
    };

    if signo as i32 == 0 {
        // POSIX existence probe: no signal is sent, only the target is resolved.
        let exists = match target {
            SignalTarget::SpecificProcess(pid) => cx.process_by_id(pid).is_some(),
            SignalTarget::ProcessGroup(pgid) => cx.processes_in_group(pgid).next().is_some(),
            SignalTarget::CallerGroup => cx
                .processes_in_group(cx.current_identity().process_group_id)
                .next()
                .is_some(),
            SignalTarget::BroadcastAll => true,
        };
        return if exists { Ok(0) } else { Err(ESRCH) };
    }

    let signal = Signal::try_from(signo as i32)?;
    sys_kill(&cx, target, signal)
}

/// Copies `value` to the userspace pointer `addr`, returning `EFAULT` unless
/// the whole range is user memory that a region backs with write access.
/// Making the range resident up front keeps a bad pointer from faulting the
/// copy-out in Ring 0 and panicking the kernel.
fn write_user<T>(addr: usize, value: T) -> Result<(), Errno> {
    let mut ptr = UserspaceMutPtr::<T>::try_from_usize(addr).map_err(|_| EFAULT)?;
    ptr.validate_range(size_of::<T>()).map_err(|_| EFAULT)?;
    make_user_range_resident(addr, size_of::<T>(), UserAccess::Write)?;
    unsafe {
        // Safety: the range is lower-half, resident, and user writable, so
        // this write cannot fault.
        ptr.as_mut_ptr().write_unaligned(value);
    }
    Ok(())
}

/// Reads a `T` from the userspace pointer `addr`, returning `EFAULT` unless
/// the whole range is user memory that a region backs with read access.
/// Making the range resident up front keeps a bad pointer from faulting the
/// copy-in in Ring 0 and panicking the kernel.
fn read_user<T>(addr: usize) -> Result<T, Errno> {
    let ptr = UserspacePtr::<T>::try_from_usize(addr).map_err(|_| EFAULT)?;
    ptr.validate_range(size_of::<T>()).map_err(|_| EFAULT)?;
    make_user_range_resident(addr, size_of::<T>(), UserAccess::Read)?;
    // Safety: the range is lower-half, resident, and user readable, so this
    // read cannot fault.
    Ok(unsafe { ptr.as_ptr().read_unaligned() })
}

fn dispatch_sys_sigaction(signo: usize, new: usize, old: usize) -> Result<usize, Errno> {
    let signal = Signal::try_from(signo as i32)?;

    let new_action = if new == 0 {
        None
    } else {
        Some(read_user::<SigAction>(new)?)
    };

    let process = Process::current();
    let old_action = process.signals_write().sigaction(signal, new_action)?;

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
        Some(read_user::<SigSet>(set)?)
    };

    let process = Process::current();
    let old_mask = process.signals_write().sigprocmask(how, new_set);

    if oldset != 0 {
        write_user(oldset, old_mask)?;
    }
    Ok(0)
}

fn dispatch_sys_sigpending(out: usize) -> Result<usize, Errno> {
    if out == 0 {
        return Err(EINVAL);
    }
    let pending = Process::current().signals_read().sigpending();
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

    let process = Process::current();
    let address_space = process.address_space();
    process
        .memory_regions()
        .populate(
            address_space,
            addr,
            len,
            matches!(access, UserAccess::Write),
        )
        .map_err(|e| match e {
            PageInError::OutOfMemory => ENOMEM,
            PageInError::MapFailed => EFAULT,
            PageInError::ReadFailed => EIO,
            PageInError::NotWritable => EFAULT,
        })?;

    let accessible = match access {
        UserAccess::Read => address_space.is_user_readable(addr, len),
        UserAccess::Write => address_space.is_user_writable(addr, len),
    };
    if accessible { Ok(()) } else { Err(EFAULT) }
}

fn dispatch_sys_getcwd(path: usize, size: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let path = UserspaceMutPtr::try_from_usize(path).map_err(|_| EFAULT)?;
    path.validate_range(size).map_err(|_| EFAULT)?;
    make_user_range_resident(path.addr(), size, UserAccess::Write)?;
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

    let addr = UserspacePtr::try_from_usize(addr)?;
    let prot = ProtFlags::from_bits(i32::try_from(prot)?).ok_or(EINVAL)?;
    let flags = MapFlags::from_bits(i32::try_from(flags)?).ok_or(EINVAL)?;
    let fd = <KernelAccess as MemoryRegionAccess>::Fd::from(i32::try_from(fd)?);
    sys_mmap(&cx, addr, len, prot, flags, fd, offset)
}

fn dispatch_sys_munmap(addr: usize, len: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let addr = UserspacePtr::try_from_usize(addr)?;
    sys_munmap(&cx, addr, len)
}

fn dispatch_sys_open(
    path: usize,
    path_len: usize,
    oflag: usize,
    mode: usize,
) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    make_user_range_resident(path, path_len, UserAccess::Read)?;
    let path = UserspacePtr::try_from_usize(path)?;
    sys_open(&cx, path, path_len, oflag as i32, mode as i32)
}

fn dispatch_sys_unlink(path: usize, path_len: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    make_user_range_resident(path, path_len, UserAccess::Read)?;
    let path = UserspacePtr::try_from_usize(path)?;
    sys_unlink(&cx, path, path_len)
}

fn dispatch_sys_read(fd: usize, buf: usize, nbyte: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let slice = unsafe { slice_from_ptr_and_len_mut(buf, nbyte) }?;
    make_user_range_resident(buf, nbyte, UserAccess::Write)?;
    sys_read(&cx, fd, slice)
}

fn dispatch_sys_getdents(fd: usize, buf: usize, len: usize) -> Result<usize, Errno> {
    debug!(fd, len, "getdents start");
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let slice = unsafe { slice_from_ptr_and_len_mut(buf, len) }?;
    make_user_range_resident(buf, len, UserAccess::Write)?;
    let result = sys_getdents(&cx, fd, slice);
    debug!(?result, "getdents end");
    result
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

fn dispatch_sys_ftruncate(fd: usize, len: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    sys_ftruncate(&cx, fd, len as i64)
}

fn dispatch_sys_close(fd: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    sys_close(&cx, fd)
}

fn dispatch_sys_fstat(fd: usize, buf: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    // The destination must be resident before the filesystem lock is taken,
    // because paging it in re-enters that same non-reentrant lock.
    make_user_range_resident(buf, size_of::<Stat>(), UserAccess::Write)?;
    let stat = cx.fstat(fd)?;
    write_user(buf, stat)?;
    Ok(0)
}

fn dispatch_sys_lseek(fd: usize, offset: usize, whence: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let whence = Whence::try_from(whence)?;
    // A negative offset arrives as its two's complement in the register.
    sys_lseek(&cx, fd, offset as i64, whence)
}

fn socket_fd(fd: usize) -> Result<<KernelAccess as SocketAccess>::Fd, Errno> {
    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    Ok(<KernelAccess as SocketAccess>::Fd::from(fd))
}

/// Returns the path inside a user socket address. The length check comes before
/// the pointer read, so a large `addrlen` cannot reach past the address.
fn user_socket_path(addr: usize, addrlen: usize) -> Result<&'static str, Errno> {
    if addrlen > size_of::<SockAddrUn>() {
        return Err(EINVAL);
    }
    let bytes = unsafe { slice_from_ptr_and_len(addr, addrlen) }?;
    make_user_range_resident(addr, addrlen, UserAccess::Read)?;
    SockAddrUn::path_of(bytes)
}

fn dispatch_sys_socket(domain: usize, ty: usize, protocol: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    sys_socket(&cx, domain as c_int, ty as c_int, protocol as c_int)
}

fn dispatch_sys_bind(fd: usize, addr: usize, addrlen: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    let path = user_socket_path(addr, addrlen)?;
    sys_bind(&cx, fd, path)
}

fn dispatch_sys_listen(fd: usize, backlog: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    sys_listen(&cx, fd, (backlog as c_int).clamp(1, SOMAXCONN) as usize)
}

fn dispatch_sys_accept(fd: usize, addr: usize, addrlen: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    let new_fd = sys_accept(&cx, fd)?;
    // The peer has no name, so the kernel writes only the family. A null `addr`
    // skips the write. The new fd stays open on `EFAULT`.
    if addr != 0 {
        let cap = read_user::<SockLen>(addrlen)?;
        if cap >= 2 {
            write_user::<SaFamily>(addr, AF_UNIX)?;
        }
        write_user::<SockLen>(addrlen, 2)?;
    }
    Ok(new_fd)
}

fn dispatch_sys_connect(fd: usize, addr: usize, addrlen: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    let path = user_socket_path(addr, addrlen)?;
    sys_connect(&cx, fd, path)
}

fn dispatch_sys_shutdown(fd: usize, how: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    sys_shutdown(&cx, fd, ShutdownHow::try_from(how as c_int)?)
}

fn user_iovecs(hdr: &MsgHdr) -> Result<Vec<IoVec>, Errno> {
    let count = hdr.iovec_count()?;
    if count == 0 {
        return Ok(Vec::new());
    }
    if hdr.msg_iov == 0 {
        return Err(EFAULT);
    }
    make_user_range_resident(hdr.msg_iov, count * size_of::<IoVec>(), UserAccess::Read)?;
    Ok((0..count)
        .map(|i| unsafe { (hdr.msg_iov as *const IoVec).add(i).read_unaligned() })
        .collect())
}

fn copy_in_user(ptr: usize, len: usize) -> Result<Vec<u8>, Errno> {
    if len == 0 {
        return Ok(Vec::new());
    }
    make_user_range_resident(ptr, len, UserAccess::Read)?;
    let src = unsafe { slice_from_ptr_and_len::<u8>(ptr, len) }?;
    Ok(src.to_vec())
}

fn copy_out_user(ptr: usize, src: &[u8]) -> Result<(), Errno> {
    if src.is_empty() {
        return Ok(());
    }
    make_user_range_resident(ptr, src.len(), UserAccess::Write)?;
    let dst = unsafe { slice_from_ptr_and_len_mut::<u8>(ptr, src.len()) }?;
    dst.copy_from_slice(src);
    Ok(())
}

/// Gathers at most `SOCKET_BUFFER_BYTES`, because a socket takes no more than
/// that in one call.
fn gather_send_bytes(hdr: &MsgHdr) -> Result<Vec<u8>, Errno> {
    let mut data = Vec::new();
    for iov in user_iovecs(hdr)? {
        let room = SOCKET_BUFFER_BYTES - data.len();
        if room == 0 {
            break;
        }
        data.extend_from_slice(&copy_in_user(iov.base, iov.len.min(room))?);
    }
    Ok(data)
}

fn dispatch_sys_sendmsg(fd: usize, msg: usize, flags: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    let flags = i32::try_from(flags).map_err(|_| EINVAL)?;
    let hdr = read_user::<MsgHdr>(msg)?;
    let control = copy_in_user(hdr.msg_control, hdr.control_len()?)?;
    let data = gather_send_bytes(&hdr)?;
    sys_sendmsg(&cx, fd, flags, &data, &control)
}

fn dispatch_sys_recvmsg(fd: usize, msg: usize, flags: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    let flags = i32::try_from(flags).map_err(|_| EINVAL)?;
    let mut hdr = read_user::<MsgHdr>(msg)?;
    let iovecs = user_iovecs(&hdr)?;
    let capacity = iovecs
        .iter()
        .fold(0usize, |sum, iov| sum.saturating_add(iov.len))
        .min(SOCKET_BUFFER_BYTES);
    let mut data = vec![0u8; capacity];
    let mut control = vec![0u8; hdr.control_len()?];
    let outcome = sys_recvmsg(&cx, fd, flags, &mut data, &mut control)?;
    let mut rest = &data[..outcome.len];
    for iov in iovecs {
        if rest.is_empty() {
            break;
        }
        let (chunk, tail) = rest.split_at(iov.len.min(rest.len()));
        copy_out_user(iov.base, chunk)?;
        rest = tail;
    }
    copy_out_user(hdr.msg_control, &control[..outcome.controllen])?;
    outcome.write_to(&mut hdr);
    write_user(msg, hdr)?;
    Ok(outcome.len)
}

/// POSIX clock_gettime backed by the HPET main counter.
fn dispatch_sys_clock_gettime(clockid: usize, tp: usize) -> Result<usize, Errno> {
    let since_boot = hpet().read().now().since_boot();
    let now = match ClockId::try_from(clockid)? {
        ClockId::Monotonic => since_boot,
        ClockId::Realtime => {
            let boot = Duration::from_secs(*crate::BOOT_TIME_SECONDS.get().ok_or(EINVAL)?);
            boot.checked_add(since_boot).ok_or(EINVAL)?
        }
    };
    write_user::<Timespec>(tp, Timespec::from(now))?;
    Ok(0)
}

/// POSIX nanosleep. Wake precision is one LAPIC tick.
fn dispatch_sys_nanosleep(req: usize, rem: usize) -> Result<usize, Errno> {
    let duration = Duration::try_from(read_user::<Timespec>(req)?)?;
    let deadline = hpet().read().now().saturating_add(duration);
    let process = Process::current().clone();

    let outcome = process.park_current_task(Some(deadline), None, || {
        hpet().read().now() >= deadline || process.signals_read().has_interrupting_deliverable()
    });

    let now = hpet().read().now();
    if now >= deadline {
        return Ok(0);
    }
    match outcome {
        ParkOutcome::Ready => {
            // POSIX allows a null rem, which skips the remainder writeback.
            if rem != 0 {
                let left = deadline.saturating_duration_since(now);
                write_user::<Timespec>(rem, Timespec::from(left))?;
            }
            Err(EINTR)
        }
        ParkOutcome::Interrupted => Err(EINTR),
    }
}

fn dispatch_sys_exe_path(buf: usize, len: usize) -> Result<usize, Errno> {
    let slice = unsafe { slice_from_ptr_and_len_mut(buf, len) }?;
    make_user_range_resident(buf, len, UserAccess::Write)?;

    let process = Process::current();
    let path = process.executable_path().ok_or(ENOENT)?;

    let bytelen = path.len();
    if slice.len() < bytelen {
        return Err(ERANGE);
    }
    slice.iter_mut().zip(path.bytes()).for_each(|(s, b)| *s = b);
    Ok(bytelen)
}
