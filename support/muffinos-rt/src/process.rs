use core::convert::Infallible;
use core::ffi::{c_char, c_int, c_long};
use core::ops::Deref;
use core::slice;

use kernel_abi::{
    E2BIG, EINVAL, ENOMEM, EOVERFLOW, Errno, SaFlags, SigAction, SigHandler, SigMaskHow, SigSet,
    Signal, WaitOptions,
};
use libc::{
    _SC_GETPW_R_SIZE_MAX, _SC_NPROCESSORS_ONLN, _SC_PAGESIZE, _SC_THREAD_STACK_MIN,
    PTHREAD_STACK_MIN, SIG_ERR, pid_t, sigaction as c_sigaction, sighandler_t, sigset_t,
};

use crate::env::environ;
use crate::errno::{error_code, fail, ret};
use crate::mem::{free, malloc};
use crate::string::c_str;

pub(crate) const PAGE_SIZE: usize = 4096;
const GETPW_R_SIZE_MAX: c_long = 1024;

#[unsafe(no_mangle)]
pub extern "C" fn exit(code: c_int) -> ! {
    muffin_sys::exit(code)
}

#[unsafe(no_mangle)]
pub extern "C" fn _exit(code: c_int) -> ! {
    muffin_sys::exit(code)
}

/// Raises `SIGABRT`. When a handler returns, exits with the status that
/// `waitpid` reports for a process that `SIGABRT` ends.
#[unsafe(no_mangle)]
pub extern "C" fn abort() -> ! {
    // A failed kill still ends the process through the exit below.
    let _ = muffin_sys::kill(muffin_sys::getpid(), Signal::Abort);
    muffin_sys::exit(128 + Signal::Abort.number())
}

fn to_pid(pid: i64) -> Result<pid_t, Errno> {
    pid_t::try_from(pid).map_err(|_| EOVERFLOW)
}

#[unsafe(no_mangle)]
pub extern "C" fn getpid() -> pid_t {
    ret(to_pid(muffin_sys::getpid()))
}

#[unsafe(no_mangle)]
pub extern "C" fn getppid() -> pid_t {
    ret(to_pid(muffin_sys::getppid()))
}

#[unsafe(no_mangle)]
pub extern "C" fn kill(pid: pid_t, signo: c_int) -> c_int {
    ret(Signal::try_from(signo)
        .and_then(|signo| muffin_sys::kill(pid.into(), signo))
        .map(|()| 0))
}

/// The child runs one thread. This thread is a copy of the calling thread of
/// `fork`.
#[unsafe(no_mangle)]
pub extern "C" fn fork() -> pid_t {
    let forked = muffin_sys::fork().and_then(to_pid);
    if forked == Ok(0) {
        crate::thread::refresh_after_fork();
    }
    ret(forked)
}

/// Strings of a NULL-terminated C string array, in a `malloc` buffer that
/// drop frees.
struct StrArray<'a> {
    strs: *mut &'a str,
    len: usize,
}

impl<'a> StrArray<'a> {
    /// # Safety
    ///
    /// `array` must be a NULL-terminated array of NUL-terminated strings that
    /// outlive `'a`.
    unsafe fn new(array: *const *const c_char) -> Result<Self, Errno> {
        let mut len = 0;
        while !unsafe { *array.add(len) }.is_null() {
            len += 1;
        }
        let bytes = len.checked_mul(size_of::<&str>()).ok_or(E2BIG)?;
        let strs = malloc(bytes).cast::<&'a str>();
        if strs.is_null() {
            return Err(ENOMEM);
        }
        let mut this = Self { strs, len: 0 };
        while this.len < len {
            let raw = unsafe { *array.add(this.len) };
            let value = unsafe { c_str(raw) }?;
            unsafe { this.strs.add(this.len).write(value) };
            this.len += 1;
        }
        Ok(this)
    }
}

impl<'a> Deref for StrArray<'a> {
    type Target = [&'a str];

    fn deref(&self) -> &Self::Target {
        // SAFETY: `new` wrote the first `len` elements.
        unsafe { slice::from_raw_parts(self.strs, self.len) }
    }
}

impl Drop for StrArray<'_> {
    fn drop(&mut self) {
        // SAFETY: `strs` came from `malloc` in `new`.
        unsafe { free(self.strs.cast()) };
    }
}

/// Returns the reason the exec failed. A successful exec never returns.
unsafe fn exec(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> Result<Infallible, Errno> {
    let path = unsafe { c_str(path) }?;
    let argv = unsafe { StrArray::new(argv) }?;
    let envp = unsafe { StrArray::new(envp) }?;
    Err(muffin_sys::execve(path, &argv, &envp))
}

/// # Safety
///
/// `path` must point to a NUL-terminated string, and `argv` and `envp` must
/// be NULL-terminated arrays of NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn execve(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    let Err(errno) = unsafe { exec(path, argv, envp) };
    fail(errno)
}

/// # Safety
///
/// Same contract as [`execve`] for `path` and `argv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn execv(path: *const c_char, argv: *const *const c_char) -> c_int {
    unsafe { execve(path, argv, environ) }
}

// shortcut: no PATH search, so `file` must be a path; upgrade when a caller
// needs bare program names.
/// # Safety
///
/// Same contract as [`execve`] for `file` and `argv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int {
    unsafe { execve(file, argv, environ) }
}

/// This function waits for a child that the POSIX `pid` forms select. It
/// returns the reaped pid. The kernel writes the status to `status` in the
/// `WEXITSTATUS` and `WTERMSIG` encoding. The function supports `WNOHANG`,
/// which returns 0 while the selected children run. Every other option bit
/// fails with `EINVAL`.
///
/// # Safety
///
/// `status` must be null or valid for one `c_int` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn waitpid(pid: pid_t, status: *mut c_int, options: c_int) -> pid_t {
    let Some(options) = WaitOptions::from_bits(options) else {
        return fail(EINVAL);
    };
    ret(unsafe { muffin_sys::waitpid(pid.into(), status, options) }.and_then(to_pid))
}

fn to_kernel_action(action: &c_sigaction) -> SigAction {
    muffin_sys::action(
        SigHandler::new(action.sa_sigaction),
        SigSet::from_bits(action.sa_mask),
        // `SA_*` are bit masks, so the cast keeps every bit.
        SaFlags::from_bits(action.sa_flags as u32),
    )
}

fn to_c_action(action: SigAction) -> c_sigaction {
    c_sigaction {
        sa_sigaction: action.handler.addr(),
        sa_mask: action.mask.bits(),
        sa_flags: action.flags.bits() as c_int,
    }
}

/// # Safety
///
/// `action` must be null or valid for one read, with `sa_sigaction` set to
/// `SIG_DFL`, `SIG_IGN` or a handler function. `old` must be null or valid
/// for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigaction(
    signo: c_int,
    action: *const c_sigaction,
    old: *mut c_sigaction,
) -> c_int {
    let new = unsafe { action.as_ref() }.map(to_kernel_action);
    let mut previous = SigAction::default();
    let result = Signal::try_from(signo)
        .and_then(|signo| muffin_sys::sigaction(signo, new.as_ref(), Some(&mut previous)));
    if result.is_ok() && !old.is_null() {
        unsafe { old.write(to_c_action(previous)) };
    }
    ret(result.map(|()| 0))
}

/// # Safety
///
/// `handler` must be `SIG_DFL`, `SIG_IGN` or a handler function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn signal(signo: c_int, handler: sighandler_t) -> sighandler_t {
    let action = c_sigaction {
        sa_sigaction: handler,
        sa_mask: 0,
        sa_flags: 0,
    };
    let mut old = c_sigaction {
        sa_sigaction: SIG_ERR,
        sa_mask: 0,
        sa_flags: 0,
    };
    if unsafe { sigaction(signo, &action, &mut old) } == 0 {
        old.sa_sigaction
    } else {
        SIG_ERR
    }
}

/// This function changes the signal mask of the calling thread.
///
/// # Safety
///
/// `set` must be null or valid for one read, and `old` must be null or valid
/// for one write.
unsafe fn change_signal_mask(
    how: c_int,
    set: *const sigset_t,
    old: *mut sigset_t,
) -> Result<(), Errno> {
    let how = SigMaskHow::try_from(how)?;
    let set = unsafe { set.as_ref() }.map(|bits| SigSet::from_bits(*bits));
    let mut previous = SigSet::EMPTY;
    muffin_sys::sigprocmask(how, set.as_ref(), Some(&mut previous))?;
    if !old.is_null() {
        unsafe { old.write(previous.bits()) };
    }
    Ok(())
}

/// This function changes the signal mask of the calling thread, like
/// [`pthread_sigmask`].
///
/// # Safety
///
/// The caller must satisfy the contract of [`change_signal_mask`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigprocmask(
    how: c_int,
    set: *const sigset_t,
    old: *mut sigset_t,
) -> c_int {
    ret(unsafe { change_signal_mask(how, set, old) }.map(|()| 0))
}

/// # Safety
///
/// The caller must satisfy the contract of [`change_signal_mask`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_sigmask(
    how: c_int,
    set: *const sigset_t,
    old: *mut sigset_t,
) -> c_int {
    error_code(unsafe { change_signal_mask(how, set, old) })
}

/// # Safety
///
/// `out` must be valid for one `sigset_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigpending(out: *mut sigset_t) -> c_int {
    let mut pending = SigSet::EMPTY;
    let result = muffin_sys::sigpending(&mut pending);
    ret(result
        .map(|()| unsafe { out.write(pending.bits()) })
        .map(|()| 0))
}

#[unsafe(no_mangle)]
pub extern "C" fn sysconf(name: c_int) -> c_long {
    match name {
        _SC_PAGESIZE => PAGE_SIZE as c_long,
        _SC_NPROCESSORS_ONLN => 1,
        _SC_GETPW_R_SIZE_MAX => GETPW_R_SIZE_MAX,
        _SC_THREAD_STACK_MIN => PTHREAD_STACK_MIN as c_long,
        _ => fail(EINVAL),
    }
}
