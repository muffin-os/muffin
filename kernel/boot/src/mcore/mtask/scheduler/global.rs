use alloc::boxed::Box;
use core::pin::Pin;

use conquer_once::spin::OnceCell;
use x86_64::instructions::interrupts;

use crate::arch::idt::InterruptIndex;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::scheduler::idle;
use crate::mcore::mtask::task::{Task, TaskQueue};

static GLOBAL_QUEUE: OnceCell<TaskQueue> = OnceCell::uninit();

fn global_queue() -> &'static TaskQueue {
    GLOBAL_QUEUE.get().unwrap()
}

/// The global run queue every CPU schedules from.
pub struct GlobalTaskQueue;

impl GlobalTaskQueue {
    pub fn init() {
        GLOBAL_QUEUE.init_once(TaskQueue::new);
    }

    pub fn enqueue(task: Pin<Box<Task>>) {
        // `dequeue` spins on a half-finished enqueue, so a tick must not
        // interrupt one. The notification must use this CPU's LAPIC.
        interrupts::without_interrupts(|| {
            global_queue().enqueue(task);
            // Safety: interrupts are off, so the context is this CPU's.
            // Early boot has no context and no registered idle CPU.
            let Some(context) = (unsafe { ExecutionContext::try_load() }) else {
                return;
            };
            if let Some(lapic_id) = idle::claim_idle(context.cpu_id()) {
                let mut lapic = context.lapic().lock_without_interrupts();
                // Safety: interrupts are off, so the LAPIC is this CPU's.
                unsafe { lapic.send_ipi(InterruptIndex::SchedulerWake.as_u8(), lapic_id) };
            }
        });
    }

    #[must_use]
    pub fn dequeue() -> Option<Pin<Box<Task>>> {
        global_queue().dequeue()
    }
}
