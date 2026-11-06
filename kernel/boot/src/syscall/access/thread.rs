use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use kernel_abi::{Errno, ThreadId};
use kernel_syscall::UserspacePtr;
use kernel_syscall::access::{ExitOutcome, ParkEnd, ThreadAccess, ThreadStart, WakeQueue};
use kernel_syscall::mman::sys_munmap;
use kernel_syscall::thread::{AddrWaitQueues, ThreadTable};
use spin::Mutex;

use crate::hpet::hpet;
use crate::mcore::mtask::process::ParkOutcome;
use crate::mcore::mtask::task::Task;
use crate::mcore::mtask::wait::WaitQueue;
use crate::syscall::access::KernelAccess;
use crate::syscall::thread::{UnstartedTask, create_thread};
use crate::syscall::user_mem::{UserAccess, with_user_slice};

impl WakeQueue for WaitQueue {
    fn wake_all(&self) {
        WaitQueue::wake_all(self);
    }
}

impl ThreadAccess for KernelAccess {
    type WakeQueue = WaitQueue;
    type UnstartedThread = UnstartedTask;

    fn current_thread(&self) -> ThreadId {
        Task::current().id().into()
    }

    fn threads(&self) -> &Mutex<ThreadTable> {
        self.process.threads()
    }

    fn thread_exits(&self) -> &WaitQueue {
        self.process.thread_exits()
    }

    fn addr_waiters(&self) -> &Mutex<AddrWaitQueues<WaitQueue>> {
        self.process.addr_waiters()
    }

    fn with_user_word<R>(
        &self,
        addr: UserspacePtr<u32>,
        f: impl FnOnce(&dyn Fn() -> u32) -> R,
    ) -> Result<R, Errno> {
        debug_assert!(addr.addr().is_multiple_of(align_of::<u32>()));
        with_user_slice(addr.addr(), size_of::<u32>(), UserAccess::Read, |bytes| {
            // Safety: the slice holds the 4-byte aligned word at `addr`.
            // Other threads of the process write it atomically.
            let word = unsafe { AtomicU32::from_ptr(bytes.as_mut_ptr().cast()) };
            f(&|| word.load(Ordering::SeqCst))
        })
    }

    fn create_thread(&self, start: ThreadStart) -> Result<UnstartedTask, Errno> {
        create_thread(&self.process, start)
    }

    fn start_thread(&self, thread: UnstartedTask) {
        thread.start();
    }

    fn park(
        &self,
        queue: &WaitQueue,
        timeout: Option<Duration>,
        mut should_wake: impl FnMut() -> bool,
    ) -> ParkEnd {
        let deadline = timeout.map(|timeout| hpet().read().now().saturating_add(timeout));
        let task = Task::current();
        let mut end = ParkEnd::Interrupted;
        let outcome = self.process.park_current_task(deadline, Some(queue), || {
            end = if should_wake() {
                ParkEnd::Woken
            } else if deadline.is_some_and(|deadline| hpet().read().now() >= deadline) {
                ParkEnd::TimedOut
            } else if task.has_interrupting_signal() {
                ParkEnd::Interrupted
            } else {
                return false;
            };
            true
        });
        match outcome {
            ParkOutcome::Ready => end,
            ParkOutcome::Interrupted => ParkEnd::Interrupted,
        }
    }

    fn unmap(&self, addr: UserspacePtr<u8>, len: usize) -> Result<(), Errno> {
        sys_munmap(self, addr, len).map(|_| ())
    }

    fn set_exit_outcome(&self, outcome: ExitOutcome) {
        self.process.set_exit_outcome(outcome);
    }
}
