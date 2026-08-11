use alloc::boxed::Box;
use core::ffi::c_void;
use core::pin::Pin;
use core::ptr;

use conquer_once::spin::OnceCell;
use kernel_park::UnparkTicketCell;
use tracing::{debug, info};
use wait::{block_current, unpark_and_enqueue};

use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::{Task, TaskQueue};
use crate::mcore::mtask::wait;

static TASK_CLEANUP_QUEUE: OnceCell<TaskQueue> = OnceCell::uninit();

static CLEANUP_UNPARK: UnparkTicketCell<Pin<Box<Task>>> = UnparkTicketCell::new();

fn task_cleanup_queue() -> &'static TaskQueue {
    TASK_CLEANUP_QUEUE.get().unwrap()
}

/// The task cleanup queue is used to store tasks that have deallocated all their
/// resources that they can (that is, everything userspace, like their stack and TLS),
/// but their kernel stack is still allocated and valid. Every task in the cleanup queue
/// will be dropped within a kernel thread and with interrupts enabled.
pub struct TaskCleanup;

impl TaskCleanup {
    pub fn init() {
        TASK_CLEANUP_QUEUE.init_once(TaskQueue::new);

        let cleanup_task = Task::create_new(Process::root(), Self::cleanup_tasks, ptr::null_mut())
            .expect("should be able to create cleanup task");
        info!(id = %cleanup_task.id(), "cleanup task created");
        GlobalTaskQueue::enqueue(Box::pin(cleanup_task));
    }

    pub fn enqueue(task: Pin<Box<Task>>) {
        task_cleanup_queue().enqueue(task);
        if let Some(ticket) = CLEANUP_UNPARK.take() {
            unpark_and_enqueue(ticket);
        }
    }

    #[must_use]
    pub fn dequeue() -> Option<Pin<Box<Task>>> {
        task_cleanup_queue().dequeue()
    }

    pub(in crate::mcore) extern "C" fn cleanup_tasks(_: *mut c_void) {
        loop {
            while let Some(task) = TaskCleanup::dequeue() {
                debug!(id = %task.id(), "dropping task");
            }
            let park_ticket = match CLEANUP_UNPARK.put_reservation(wait::reserve()) {
                Ok(ticket) => ticket,
                Err(reservation) => {
                    reservation.release();
                    continue;
                }
            };
            // A corpse enqueued before the ticket was stored found nothing to
            // wake. Consuming the ticket makes the park below bounce.
            if let Some(task) = TaskCleanup::dequeue() {
                debug!(id = %task.id(), "dropping task");
                if let Some(ticket) = CLEANUP_UNPARK.take() {
                    unpark_and_enqueue(ticket);
                }
            }
            block_current(park_ticket);
        }
    }
}
