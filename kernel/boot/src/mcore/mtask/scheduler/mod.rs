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
use crate::mcore::mtask::task::Task;

pub mod cleanup;
pub mod global;
pub mod stopped;
mod switch;

/// Where an outgoing task goes once it is off the CPU.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Disposal {
    Enqueue,
    Park,
    Terminate,
}

#[derive(Debug)]
pub struct Scheduler {
    current_task: Pin<Box<Task>>,
    /// The task last switched away from, with the disposal decided when its
    /// RSP save slot was picked, so routing matches whether RSP was saved.
    zombie_task: Option<(Pin<Box<Task>>, Disposal)>,
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

        if let Some((zombie_task, disposal)) = self.zombie_task.take() {
            Self::route(zombie_task, disposal);
        }

        let requested = if self.current_task.take_should_park() {
            Disposal::Park
        } else {
            Disposal::Enqueue
        };
        // A parking task cannot stay on the CPU, so an idle task is an
        // acceptable target even while the run queue is empty.
        let must_switch = matches!(requested, Disposal::Park);

        let Some(next_task) = self.next_task(must_switch) else {
            return;
        };
        let cr3_value = next_task.process().address_space().cr3_value();

        let mut old_task = self.swap_current_task(next_task);
        let disposal = if old_task.should_terminate() {
            Disposal::Terminate
        } else {
            requested
        };
        let old_stack_ptr = if matches!(disposal, Disposal::Terminate) {
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
        self.zombie_task = Some((old_task, disposal));

        // Point TSS.RSP0 at the incoming task's kernel stack so its next
        // Ring 3 to Ring 0 transition lands on a per-task stack.
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

    fn route(task: Pin<Box<Task>>, disposal: Disposal) {
        match disposal {
            Disposal::Enqueue => GlobalTaskQueue::enqueue(task),
            Disposal::Park => Self::park(task),
            Disposal::Terminate => TaskCleanup::enqueue(task),
        }
    }

    /// SIGCONT clears the flag and drains the lot under the signals write
    /// guard, so the flag is read here with the guard held across the insert.
    fn park(task: Pin<Box<Task>>) {
        let process = task.process().clone();
        match process.try_signals_read() {
            Some(guard) if guard.stopped() => StoppedTasks::park(task),
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
    /// An idle task is only taken when the current task cannot continue.
    fn next_task(&self, must_switch: bool) -> Option<Pin<Box<Task>>> {
        if let Some(task) = GlobalTaskQueue::dequeue() {
            return Some(task);
        }
        if must_switch || self.current_task.is_idle() || self.current_task.should_terminate() {
            return GlobalTaskQueue::dequeue_idle();
        }
        None
    }
}
