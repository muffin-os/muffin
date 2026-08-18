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
use crate::mcore::mtask::scheduler::switch::switch_impl;
use crate::mcore::mtask::task::{Task, TaskId};
use crate::mcore::mtask::wait::TaskParkTicket;

pub mod cleanup;
pub mod global;
mod switch;

/// Where an outgoing task goes once it is off the CPU.
#[derive(Debug)]
enum Disposal {
    Enqueue,
    Park(TaskParkTicket),
    Terminate,
}

#[derive(Debug)]
pub struct Scheduler {
    current_task: Pin<Box<Task>>,
    /// This CPU's own idle task while it is off the CPU. It never enters the
    /// global queue, so it cannot migrate and a reschedule that must switch
    /// always finds a target.
    idle_task: Option<Pin<Box<Task>>>,
    idle_tid: Option<TaskId>,
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
            idle_tid: None,
            current_task,
            idle_task: None,
            zombie_task: None,
            dummy_old_stack_ptr: UnsafeCell::new(0),
        }
    }

    /// # Safety
    /// Trivially unsafe. If you don't know why, please don't call this function.
    pub unsafe fn reschedule(&mut self) {
        assert!(!interrupts::are_enabled());

        if let Some((zombie_task, disposal)) = self.zombie_task.take() {
            self.route(zombie_task, disposal);
        }

        // A parking task cannot stay on the CPU, so an idle task is an
        // acceptable target even while the run queue is empty. The ticket is
        // taken only after a switch target exists, because an early return
        // with a taken ticket would drop it and leak its slot.
        let must_switch = self.current_task.has_park_ticket();

        let Some(next_task) = self.next_task(must_switch) else {
            return;
        };
        let cr3_value = next_task.process().address_space().cr3_value();

        let mut old_task = self.swap_current_task(next_task);
        let requested = match old_task.take_park_ticket() {
            Some(ticket) => Disposal::Park(ticket),
            None => Disposal::Enqueue,
        };
        // A pending park ticket wins over termination. Every ticket has a
        // live wake source, and `should_terminate` can only be set while the
        // task runs, so a parked task is always woken first and terminated at
        // its next reschedule. Dropping the ticket here would leak the slot.
        let disposal = if old_task.should_terminate() && !matches!(requested, Disposal::Park(_)) {
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

    fn route(&mut self, task: Pin<Box<Task>>, disposal: Disposal) {
        match disposal {
            Disposal::Enqueue => {
                if self.idle_tid == Some(task.id()) {
                    self.idle_task = Some(task);
                } else {
                    GlobalTaskQueue::enqueue(task);
                }
            }
            Disposal::Park(ticket) => match ticket.park(task) {
                Ok(()) => {}
                Err(err) => GlobalTaskQueue::enqueue(err.into_inner()),
            },
            Disposal::Terminate => TaskCleanup::enqueue(task),
        }
    }

    pub fn set_idle_task(&mut self, task: Pin<Box<Task>>) {
        assert!(!interrupts::are_enabled());
        self.idle_tid = Some(task.id());
        self.idle_task = Some(task);
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
    /// The own idle task is only taken when the current task cannot continue.
    fn next_task(&mut self, must_switch: bool) -> Option<Pin<Box<Task>>> {
        GlobalTaskQueue::dequeue().or_else(|| {
            (must_switch || self.current_task.should_terminate())
                .then(|| self.idle_task.take())
                .flatten()
        })
    }
}
