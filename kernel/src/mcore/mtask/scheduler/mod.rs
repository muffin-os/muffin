use alloc::boxed::Box;
use core::arch::asm;
use core::arch::x86_64::_fxsave;
use core::cell::UnsafeCell;
use core::mem::swap;
use core::pin::Pin;

use cleanup::TaskCleanup;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::registers::model_specific::FsBase;

use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::scheduler::switch::switch_impl;
use crate::mcore::mtask::task::{ShouldTerminate, Task};

pub mod cleanup;
pub mod global;
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
                GlobalTaskQueue::enqueue(zombie_task);
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

        if let Some(mut guard) = old_task.fx_area().try_write()
            && let Some(fx_area) = guard.as_mut()
        {
            unsafe { asm!("clts") };
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

    #[allow(clippy::unused_self)]
    fn next_task(&self) -> Option<Pin<Box<Task>>> {
        GlobalTaskQueue::dequeue()
    }
}
