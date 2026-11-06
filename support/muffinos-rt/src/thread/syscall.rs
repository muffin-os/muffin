//! This module has typed wrappers of the thread syscalls in the muffin thread ABI.

use core::ptr;
use core::sync::atomic::AtomicU32;
use core::time::Duration;

use kernel_abi::{
    EINTR, Errno, SYS_ADDR_WAIT, SYS_ADDR_WAKE, SYS_THREAD_CREATE, SYS_THREAD_EXIT,
    SYS_THREAD_JOIN, SYS_THREAD_SELF, THREAD_ENTRY_STACK_REMAINDER, THREAD_STACK_ALIGN,
    ThreadExitFlags, Timespec,
};
use libc::pthread_t;
use muffin_sys::{ret, syscall0, syscall1, syscall2, syscall3};

use super::Stack;

/// This function starts a thread of this process. The thread runs
/// `entry(arg)` with `rsp = stack_top`. The function returns the tid of the
/// thread.
///
/// # Safety
///
/// `stack_top` must name an unused mapped stack and satisfy [`THREAD_ENTRY_STACK_REMAINDER`].
/// `arg` must stay valid for `entry`.
pub(super) unsafe fn thread_create<T>(
    entry: extern "C" fn(*const T) -> !,
    arg: *const T,
    stack_top: usize,
) -> Result<pthread_t, Errno> {
    debug_assert_eq!(stack_top % THREAD_STACK_ALIGN, THREAD_ENTRY_STACK_REMAINDER);
    ret(unsafe { syscall3(SYS_THREAD_CREATE, entry as usize, arg as usize, stack_top) })
}

/// This function ends the calling thread. When `unmap` is `Some`, the kernel
/// unmaps that stack first. The unmap happens after the thread stops using the
/// stack.
///
/// # Safety
///
/// No other thread may use `unmap`.
pub(super) unsafe fn thread_exit(flags: ThreadExitFlags, unmap: Option<Stack>) -> ! {
    let (addr, len) = unmap.map_or((0, 0), |stack| (stack.base as usize, stack.len));
    let raw = unsafe { syscall3(SYS_THREAD_EXIT, flags.bits(), addr, len) };
    panic!("SYS_THREAD_EXIT returned {:?}", ret(raw))
}

/// This function blocks until the thread `tid` exits. It then removes the exit
/// record. A signal does not end the wait.
pub(super) fn thread_join(tid: pthread_t) -> Result<(), Errno> {
    loop {
        match ret(unsafe { syscall1(SYS_THREAD_JOIN, tid) }) {
            Err(EINTR) => {}
            joined => return joined.map(drop),
        }
    }
}

pub(super) fn thread_self() -> pthread_t {
    unsafe { syscall0(SYS_THREAD_SELF) }
}

/// This function parks the calling thread while `addr` holds `expected`. The
/// park ends on a wake on `addr`, at the end of `timeout` or on a signal.
///
/// # Errors
///
/// The function returns `EAGAIN` when `addr` does not hold `expected`. It
/// returns `ETIMEDOUT` when `timeout` elapses. It returns `EINTR` when a
/// signal ends the wait.
pub(super) fn addr_wait(
    addr: &AtomicU32,
    expected: u32,
    timeout: Option<Duration>,
) -> Result<(), Errno> {
    let timeout = timeout.map(Timespec::from);
    let timeout = timeout
        .as_ref()
        .map_or(0, |timeout| ptr::from_ref(timeout) as usize);
    ret(unsafe {
        syscall3(
            SYS_ADDR_WAIT,
            addr.as_ptr() as usize,
            expected as usize,
            timeout,
        )
    })
    .map(drop)
}

/// This function wakes up to `count` threads that wait on `addr`. It returns
/// the number of woken threads.
pub(super) fn addr_wake(addr: &AtomicU32, count: usize) -> Result<usize, Errno> {
    ret(unsafe { syscall2(SYS_ADDR_WAKE, addr.as_ptr() as usize, count) })
}
