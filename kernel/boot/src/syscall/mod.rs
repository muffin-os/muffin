use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_int;
use core::ops::Neg;
use core::time::Duration;

use access::KernelAccess;
use kernel_abi::{
    AF_UNIX, ClockId, EBADF, EFAULT, EINTR, EINVAL, ENOENT, ENOSYS, ERANGE, ESRCH, Errno, IoVec,
    IoctlRequest, MapFlags, MsgHdr, PATH_MAX, POLL_TIMEOUT_INFINITE, PollFd, ProcessId, ProtFlags,
    SOMAXCONN, SaFamily, ShutdownHow, SigAction, SigMaskHow, SigSet, Signal, SockAddrUn, SockLen,
    Timespec, WaitOptions, Whence, syscall_name,
};
use kernel_syscall::UserspacePtr;
use kernel_syscall::access::{
    FileAccess, MemoryRegionAccess, PermissionAccess, ProcessesAccess, SocketAccess,
};
use kernel_syscall::fcntl::{sys_open, sys_unlink};
use kernel_syscall::mman::{sys_mmap, sys_mprotect, sys_munmap};
use kernel_syscall::poll::sys_poll;
use kernel_syscall::signal::{SignalTarget, sys_kill, sys_sigprocmask};
use kernel_syscall::socket::{
    sys_accept, sys_bind, sys_connect, sys_listen, sys_recvmsg, sys_sendmsg, sys_shutdown,
    sys_socket,
};
use kernel_syscall::unistd::{
    sys_close, sys_dup2, sys_dupfd, sys_fsync, sys_ftruncate, sys_getcwd, sys_getdents, sys_ioctl,
    sys_lseek, sys_read, sys_write,
};
use kernel_syscall::wait::{WaitTarget, sys_getppid, sys_waitpid};
use kernel_unix_socket::SOCKET_BUFFER_BYTES;
use tracing::{error, trace};
use x86_64::structures::idt::InterruptStackFrame;

use self::user_mem::{
    MAX_USER_COPY_BYTES, bounce_buffer, check_user_range, copy_in, copy_out, read_user,
    read_user_array, write_user, write_user_array,
};
use crate::arch::idt::SyscallRegisters;
use crate::hpet::hpet;
use crate::mcore::mtask::process::{ExitOutcome, ParkOutcome, Process};
use crate::mcore::mtask::task::Task;

pub mod access;
mod exec;
mod fork;
mod thread;
pub(crate) mod user_mem;

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
        kernel_abi::SYS_DUP2 => dispatch_sys_dup2(arg1, arg2),
        kernel_abi::SYS_DUPFD => dispatch_sys_dupfd(arg1, arg2),
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
        kernel_abi::SYS_MPROTECT => dispatch_sys_mprotect(arg1, arg2, arg3),
        kernel_abi::SYS_POLL => dispatch_sys_poll(arg1, arg2, arg3),
        kernel_abi::SYS_THREAD_CREATE => thread::dispatch_sys_thread_create(arg1, arg2, arg3),
        kernel_abi::SYS_THREAD_EXIT => thread::dispatch_sys_thread_exit(arg1, arg2, arg3),
        kernel_abi::SYS_THREAD_JOIN => thread::dispatch_sys_thread_join(arg1),
        kernel_abi::SYS_THREAD_SELF => thread::dispatch_sys_thread_self(),
        kernel_abi::SYS_ADDR_WAIT => thread::dispatch_sys_addr_wait(arg1, arg2, arg3),
        kernel_abi::SYS_ADDR_WAKE => thread::dispatch_sys_addr_wake(arg1, arg2),
        _ => {
            error!("unknown syscall number {n}");
            Err(ENOSYS)
        }
    };

    match result {
        Ok(ret) => ret as isize,
        Err(e) => {
            trace!("syscall {} ({n}) failed with error: {e:?}", syscall_name(n));
            Into::<isize>::into(e).neg()
        }
    }
}

fn dispatch_sys_exit(code: usize) -> Result<usize, Errno> {
    let process = Process::current();
    trace!("process {} exit with code {code}", process.pid());
    // The other threads end before the outcome becomes visible. Therefore a
    // parent that sees the exit never races a thread of the child that still
    // runs.
    let _sole = process.reap_sibling_tasks(Task::current().id());
    process.set_exit_outcome(ExitOutcome::Exited(code));
    Task::exit_current()
}

fn dispatch_sys_poweroff() -> Result<usize, Errno> {
    trace!("process {} requested poweroff", Process::current().pid());
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

    let target = match pid as isize {
        1.. => WaitTarget::Child(ProcessId::from(pid as u64)),
        0 => WaitTarget::CallerGroup,
        -1 => WaitTarget::AnyChild,
        v => WaitTarget::ProcessGroup(ProcessId::from(v.unsigned_abs() as u64)),
    };
    let options = WaitOptions::from_bits(i32::try_from(options)?).ok_or(EINVAL)?;
    let Some(reaped) = sys_waitpid(&cx, target, options)? else {
        return Ok(0);
    };
    // POSIX allows a null status. In that case, the code skips the status writeback.
    if status != 0 {
        write_user(status, reaped.outcome.wait_status())?;
    }
    Ok(reaped.pid.as_u64() as usize)
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

    let old_mask = sys_sigprocmask(&KernelAccess::new(), how, new_set);

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

fn dispatch_sys_getcwd(path: usize, size: usize) -> Result<usize, Errno> {
    // A zero `size` gives `EINVAL` from `sys_getcwd`, whatever `path` is.
    if size != 0 {
        check_user_range(path, size)?;
    }
    let mut buf = bounce_buffer(size);
    let len = sys_getcwd(&KernelAccess::new(), &mut buf)?;
    copy_out(path, &buf[..len])?;
    Ok(path)
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

fn dispatch_sys_mprotect(addr: usize, len: usize, prot: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let addr = UserspacePtr::try_from_usize(addr)?;
    let prot = ProtFlags::from_bits(i32::try_from(prot)?).ok_or(EINVAL)?;
    sys_mprotect(&cx, addr, len, prot)
}

/// This function copies in at most `PATH_MAX + 1` bytes of a path. The extra
/// byte lets `sys_open` and `sys_unlink` detect a path that is too long.
fn copy_in_path(path: usize, path_len: usize) -> Result<Vec<u8>, Errno> {
    copy_in(path, path_len.min(PATH_MAX + 1))
}

fn dispatch_sys_open(
    path: usize,
    path_len: usize,
    oflag: usize,
    mode: usize,
) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let path = copy_in_path(path, path_len)?;
    sys_open(&cx, &path, oflag as i32, mode as i32)
}

fn dispatch_sys_unlink(path: usize, path_len: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let path = copy_in_path(path, path_len)?;
    sys_unlink(&cx, &path)
}

fn dispatch_sys_read(fd: usize, buf: usize, nbyte: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    if nbyte == 0 {
        cx.fstat(fd)?;
        return Ok(0);
    }
    // The check rejects an invalid buffer before the read consumes data.
    // It also rejects an invalid buffer when the read returns zero bytes.
    check_user_range(buf, nbyte)?;
    let mut data = bounce_buffer(nbyte);
    let read = sys_read(&cx, fd, &mut data)?;
    copy_out(buf, &data[..read])?;
    Ok(read)
}

fn dispatch_sys_getdents(fd: usize, buf: usize, len: usize) -> Result<usize, Errno> {
    trace!(fd, len, "getdents start");
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    check_user_range(buf, len)?;
    let mut data = bounce_buffer(len);
    let result = sys_getdents(&cx, fd, &mut data)
        .and_then(|written| copy_out(buf, &data[..written]).map(|()| written));
    trace!(?result, "getdents end");
    result
}

fn dispatch_sys_write(fd: usize, buf: usize, nbyte: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let data = copy_in(buf, nbyte.min(MAX_USER_COPY_BYTES))?;
    sys_write(&cx, fd, &data)
}

/// Runs POSIX poll. The `timeout` argument is a number of milliseconds as
/// `isize`, or [`POLL_TIMEOUT_INFINITE`].
fn dispatch_sys_poll(fds: usize, nfds: usize, timeout: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let timeout = match timeout as isize {
        POLL_TIMEOUT_INFINITE => None,
        ms @ 0.. => Some(Duration::from_millis(ms as u64)),
        _ => return Err(EINVAL),
    };
    // A call with no entries is valid. In that case, poll only sleeps for
    // `timeout`. POSIX permits `EINVAL` for an `nfds` above the descriptor
    // limit.
    if nfds > MAX_USER_COPY_BYTES / size_of::<PollFd>() {
        return Err(EINVAL);
    }
    let mut entries = read_user_array::<PollFd>(fds, nfds)?;
    let ready = sys_poll(&cx, &mut entries, timeout)?;
    write_user_array(fds, &entries)?;
    Ok(ready)
}

fn dispatch_sys_ioctl(fd: usize, request: usize, argp: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();

    let fd = i32::try_from(fd).map_err(|_| EINVAL)?;
    let fd = <KernelAccess as FileAccess>::Fd::from(fd);

    let request = IoctlRequest::try_from(request)?;
    let mut arg = copy_in(argp, request.arg_size())?;
    let result = sys_ioctl(&cx, fd, request, &mut arg)?;
    copy_out(argp, &arg)?;
    Ok(result)
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

/// Userspace passes a `c_int` sign-extended into the register.
fn c_int_arg(arg: usize, invalid: Errno) -> Result<c_int, Errno> {
    c_int::try_from(arg as isize).map_err(|_| invalid)
}

fn dispatch_sys_dup2(fildes: usize, fildes2: usize) -> Result<usize, Errno> {
    sys_dup2(
        &KernelAccess::new(),
        c_int_arg(fildes, EBADF)?,
        c_int_arg(fildes2, EBADF)?,
    )
}

fn dispatch_sys_dupfd(fildes: usize, min: usize) -> Result<usize, Errno> {
    sys_dupfd(
        &KernelAccess::new(),
        c_int_arg(fildes, EBADF)?,
        c_int_arg(min, EINVAL)?,
    )
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

/// This function copies in a user socket address. The length check comes
/// before the copy. Therefore a large `addrlen` cannot reach past the address.
fn user_socket_addr(addr: usize, addrlen: usize) -> Result<Vec<u8>, Errno> {
    if addrlen > size_of::<SockAddrUn>() {
        return Err(EINVAL);
    }
    copy_in(addr, addrlen)
}

fn dispatch_sys_socket(domain: usize, ty: usize, protocol: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    sys_socket(&cx, domain as c_int, ty as c_int, protocol as c_int)
}

fn dispatch_sys_bind(fd: usize, addr: usize, addrlen: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    let addr = user_socket_addr(addr, addrlen)?;
    sys_bind(&cx, fd, SockAddrUn::path_of(&addr)?)
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
    let addr = user_socket_addr(addr, addrlen)?;
    sys_connect(&cx, fd, SockAddrUn::path_of(&addr)?)
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
    read_user_array::<IoVec>(hdr.msg_iov, count)
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
        data.extend_from_slice(&copy_in(iov.base, iov.len.min(room))?);
    }
    Ok(data)
}

fn dispatch_sys_sendmsg(fd: usize, msg: usize, flags: usize) -> Result<usize, Errno> {
    let cx = KernelAccess::new();
    let fd = socket_fd(fd)?;
    let flags = i32::try_from(flags).map_err(|_| EINVAL)?;
    let hdr = read_user::<MsgHdr>(msg)?;
    let control = copy_in(hdr.msg_control, hdr.control_len()?)?;
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
    // The checks reject invalid buffers before the receive consumes data.
    // They also reject invalid buffers when the receive returns zero bytes.
    for iov in iovecs.iter().filter(|iov| iov.len > 0) {
        check_user_range(iov.base, iov.len)?;
    }
    if !control.is_empty() {
        check_user_range(hdr.msg_control, control.len())?;
    }
    let outcome = sys_recvmsg(&cx, fd, flags, &mut data, &mut control)?;
    let mut rest = &data[..outcome.len];
    for iov in iovecs {
        if rest.is_empty() {
            break;
        }
        let (chunk, tail) = rest.split_at(iov.len.min(rest.len()));
        copy_out(iov.base, chunk)?;
        rest = tail;
    }
    copy_out(hdr.msg_control, &control[..outcome.controllen])?;
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
    let task = Task::current();

    let outcome = task.process().park_current_task(Some(deadline), &[], || {
        hpet().read().now() >= deadline || task.has_interrupting_signal()
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
    if buf == 0 || len == 0 {
        return Err(EINVAL);
    }
    let path = Process::current().executable_path().ok_or(ENOENT)?;
    if len < path.len() {
        return Err(ERANGE);
    }
    copy_out(buf, path.as_bytes())?;
    Ok(path.len())
}
