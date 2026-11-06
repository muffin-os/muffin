use core::time::Duration;

use kernel_abi::{Errno, ThreadId};
use spin::Mutex;

use crate::UserspacePtr;
use crate::access::ExitOutcome;
use crate::signal::SignalMask;
use crate::thread::{AddrWaitQueues, ThreadTable};

/// A parked thread registers with this wake source.
pub trait WakeQueue: Default {
    /// This method unparks every thread that waits on this queue. A thread
    /// that no longer parks ignores the wake.
    fn wake_all(&self);
}

/// This type gives the reason for the return of [`ThreadAccess::park`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ParkEnd {
    /// The wake condition is true.
    Woken,
    /// The timeout elapses before the wake condition is true.
    TimedOut,
    /// A signal that the calling thread does not block interrupts the wait,
    /// or the end of the process interrupts the wait.
    Interrupted,
}

/// This type holds the user-mode start state of a new thread.
#[derive(Debug, Copy, Clone)]
pub struct ThreadStart {
    pub entry: UserspacePtr<u8>,
    /// This field holds the value of `rdi` at `entry`.
    pub arg: usize,
    pub stack_top: UserspacePtr<u8>,
    pub signal_mask: SignalMask,
}

/// A thread of this type exists but does not run yet.
pub trait UnstartedThread {
    fn id(&self) -> ThreadId;
}

/// This trait gives access to the threads of the calling process and to the
/// kernel services that the thread syscalls use.
pub trait ThreadAccess {
    type WakeQueue: WakeQueue;
    type UnstartedThread: UnstartedThread;

    fn current_thread(&self) -> ThreadId;

    /// This method returns the live threads and the exit records of the calling process.
    fn threads(&self) -> &Mutex<ThreadTable>;

    /// This method returns the queue that each thread exit of the calling process wakes.
    fn thread_exits(&self) -> &Self::WakeQueue;

    fn addr_waiters(&self) -> &Mutex<AddrWaitQueues<Self::WakeQueue>>;

    /// This method keeps the `u32` at `addr` mapped while `f` runs. It gives
    /// `f` a load of that word. `f` must not block. `addr` is 4-byte aligned.
    ///
    /// # Errors
    /// This method returns `EFAULT` when `addr` is not readable user memory.
    fn with_user_word<R>(
        &self,
        addr: UserspacePtr<u32>,
        f: impl FnOnce(&dyn Fn() -> u32) -> R,
    ) -> Result<R, Errno>;

    /// This method creates a thread of the calling process. The thread has a
    /// fresh TLS block, a fresh FPU state and a kernel stack, but no user stack.
    ///
    /// # Errors
    /// This method returns `EAGAIN` when the kernel cannot allocate the resources of the thread.
    fn create_thread(&self, start: ThreadStart) -> Result<Self::UnstartedThread, Errno>;

    fn start_thread(&self, thread: Self::UnstartedThread);

    /// This method parks the calling thread on `queue`. The park ends when
    /// `should_wake` returns true, when `timeout` elapses, or when an
    /// interruption occurs. The method does not call `should_wake` again after
    /// it returns true, so `should_wake` can consume data. A wake of `queue`
    /// only makes the thread check `should_wake` again.
    fn park(
        &self,
        queue: &Self::WakeQueue,
        timeout: Option<Duration>,
        should_wake: impl FnMut() -> bool,
    ) -> ParkEnd;

    /// This method removes the mapping `[addr, addr + len)` from the calling
    /// process, as `munmap` does.
    ///
    /// # Errors
    /// This method returns each error of `munmap` for the range.
    fn unmap(&self, addr: UserspacePtr<u8>, len: usize) -> Result<(), Errno>;

    /// This method records how the calling process ends. The first outcome wins.
    fn set_exit_outcome(&self, outcome: ExitOutcome);
}
