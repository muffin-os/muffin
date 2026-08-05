use alloc::boxed::Box;
use core::arch::x86_64::_fxsave;
use core::cell::UnsafeCell;
use core::mem::swap;
use core::pin::Pin;

use cleanup::TaskCleanup;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::registers::control::{Cr0, Cr0Flags};
use x86_64::registers::model_specific::FsBase;

use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::scheduler::stopped::StoppedTasks;
use crate::mcore::mtask::scheduler::switch::switch_impl;
use crate::mcore::mtask::task::{ShouldTerminate, Task};

pub mod cleanup;
pub mod global;
pub mod stopped;
mod switch;

#[derive(Debug)]
pub struct Scheduler {
    /// The task that is currently executing in this scheduler.
    current_task: Pin<Box<Task>>,
    /// The task this scheduler last switched away from, paired with the
    /// termination decision taken at switch time. We need this to eliminate
    /// the race condition between re-queueing a task and actually switching
    /// away from it. The flag is the snapshot of `should_terminate()` taken
    /// when we picked the old task's stack-pointer slot, so the routing
    /// decision on the next reschedule is guaranteed consistent with whether
    /// the task's RSP was actually saved.
    zombie_task: Option<(Pin<Box<Task>>, ShouldTerminate)>,
    /// A dummy location that is a placeholder for the switch code to write the old stack
    /// pointer to if the old task is terminated.
    dummy_old_stack_ptr: UnsafeCell<usize>,
}

impl Scheduler {
    #[must_use]
    pub fn new_cpu_local() -> Self {
        let current_task = Box::pin(unsafe { Task::create_current() });
        Self {
            current_task,
            zombie_task: None,
            dummy_old_stack_ptr: UnsafeCell::new(0),
        }
    }

    /// # Safety
    /// Trivially unsafe. If you don't know why, please don't call this function.
    pub unsafe fn reschedule(&mut self) {
        assert!(!interrupts::are_enabled());

        // in theory, we could move this to the end of this function, but I'd rather not do this right now
        // Route the previous zombie based on the snapshot we took when we
        // chose its stack-pointer slot — NOT a fresh load of
        // should_terminate. This keeps the routing decision consistent with
        // whether we actually saved its RSP, even if the flag flips later.
        if let Some((zombie_task, terminate)) = self.zombie_task.take() {
            if terminate.yes() {
                TaskCleanup::enqueue(zombie_task);
            } else {
                Self::route_runnable(zombie_task);
            }
        }

        let (next_task, cr3_value) = {
            let Some(next_task) = self.next_task() else {
                return;
            };

            let cr3_value = next_task.process().address_space().cr3_value();
            (next_task, cr3_value)
        };

        let mut old_task = self.swap_current_task(next_task);
        let terminate_old = old_task.should_terminate();
        let old_stack_ptr = if terminate_old.yes() {
            self.dummy_old_stack_ptr.get()
        } else {
            old_task.last_stack_ptr() as *mut usize
        };

        if !Cr0::read().contains(Cr0Flags::TASK_SWITCHED)
            && let Some(mut guard) = old_task.fx_area().try_write()
            && let Some(fx_area) = guard.as_mut()
        {
            unsafe {
                // Safety: Safe because we hold a mutable reference to the fx_area
                _fxsave(fx_area.start().as_mut_ptr::<u8>());
            }
        }

        if let Some(guard) = self.current_task.tls().try_read()
            && let Some(tls) = guard.as_ref()
        {
            FsBase::write(tls.start());
        } else {
            FsBase::write(VirtAddr::zero());
        }

        assert!(self.zombie_task.is_none());
        self.zombie_task = Some((old_task, terminate_old));

        // Point TSS.RSP0 at the incoming task's own kernel stack so its next
        // Ring 3 -> Ring 0 transition lands on a per-task stack. Without this,
        // every CPU would funnel `int 0x80` onto a single shared stack and
        // mid-syscall preemption could let one task overwrite another's frames.
        if let Some(kstack) = self.current_task.kstack() {
            ExecutionContext::load().set_kernel_stack(kstack.top());
        }

        unsafe {
            Self::switch(
                old_stack_ptr,
                *self.current_task.last_stack_ptr(),
                cr3_value,
            );
        }
    }

    /// Routes a non-terminating outgoing task either to the stopped-task
    /// parking lot (when its process is stopped by a signal) or back to the
    /// global run queue.
    ///
    /// On any contention the task goes back to the run queue. That is safe because
    /// the stopped flag persists and the task re-parks on a later tick.
    fn route_runnable(task: Pin<Box<Task>>) {
        // Keep the signals read guard alive across the park insert. A
        // concurrent SIGCONT resumes under the signals write guard,
        // so it either sees the task parked or waits until parking finished.
        // No lost wakeup.
        let process = task.process().clone();
        match process.signals().try_read() {
            Some(guard) if guard.stopped() => {
                let pid = process.pid();
                if let Err(task) = StoppedTasks::try_park(pid, task) {
                    drop(guard);
                    GlobalTaskQueue::enqueue(task);
                }
            }
            _ => GlobalTaskQueue::enqueue(task),
        }
    }

    unsafe fn switch(old_stack_ptr: *mut usize, new_stack_ptr: usize, new_cr3_value: usize) {
        unsafe {
            switch_impl(old_stack_ptr, new_stack_ptr as *const u8, new_cr3_value);
        }
    }

    #[must_use]
    pub fn current_task(&self) -> &Task {
        &self.current_task
    }

    fn swap_current_task(&mut self, next_task: Pin<Box<Task>>) -> Pin<Box<Task>> {
        let mut next_task = next_task;
        swap(&mut self.current_task, &mut next_task);
        next_task
    }

    /// Picks the task to run next, or `None` to keep running the current one.
    ///
    /// An idle task is only taken when the current task cannot continue, meaning
    /// it is itself idle or is terminating. Yielding to a halt loop while the
    /// current task is still runnable costs that task a full timer quantum.
    ///
    /// A task that never blocks starves the idle queue, so the task cleanup reaper
    /// only drops dead tasks once the core has slack.
    fn next_task(&self) -> Option<Pin<Box<Task>>> {
        if let Some(task) = GlobalTaskQueue::dequeue() {
            return Some(task);
        }
        if self.current_task.is_idle() || self.current_task.should_terminate().yes() {
            return GlobalTaskQueue::dequeue_idle();
        }
        None
    }
}
