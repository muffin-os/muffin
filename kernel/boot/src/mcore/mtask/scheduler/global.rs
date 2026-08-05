use alloc::boxed::Box;
use core::pin::Pin;

use conquer_once::spin::OnceCell;

use crate::mcore::mtask::task::{Task, TaskQueue};

static GLOBAL_QUEUE: OnceCell<TaskQueue> = OnceCell::uninit();
static IDLE_QUEUE: OnceCell<TaskQueue> = OnceCell::uninit();

fn global_queue() -> &'static TaskQueue {
    GLOBAL_QUEUE.get().unwrap()
}

fn idle_queue() -> &'static TaskQueue {
    IDLE_QUEUE.get().unwrap()
}

/// The run queues, split by priority.
///
/// [`GlobalTaskQueue::enqueue`] routes on [`Task::is_idle`] at the moment of the
/// call. Demoting a task that is already queued leaves it in the ordinary queue,
/// where it keeps taking a slice from runnable tasks.
pub struct GlobalTaskQueue;

impl GlobalTaskQueue {
    pub fn init() {
        GLOBAL_QUEUE.init_once(TaskQueue::new);
        IDLE_QUEUE.init_once(TaskQueue::new);
    }

    pub fn enqueue(task: Pin<Box<Task>>) {
        if task.is_idle() {
            idle_queue().enqueue(task);
        } else {
            global_queue().enqueue(task);
        }
    }

    #[must_use]
    pub fn dequeue() -> Option<Pin<Box<Task>>> {
        global_queue().dequeue()
    }

    /// Dequeues a task that may only run when nothing ordinary is runnable.
    #[must_use]
    pub fn dequeue_idle() -> Option<Pin<Box<Task>>> {
        idle_queue().dequeue()
    }
}
