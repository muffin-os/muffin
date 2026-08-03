use alloc::boxed::Box;
use core::pin::Pin;

use conquer_once::spin::OnceCell;

use crate::mcore::mtask::task::{Task, TaskQueue};

static GLOBAL_QUEUE: OnceCell<TaskQueue> = OnceCell::uninit();

fn global_queue() -> &'static TaskQueue {
    GLOBAL_QUEUE.get().unwrap()
}

pub struct GlobalTaskQueue;

impl GlobalTaskQueue {
    pub fn init() {
        GLOBAL_QUEUE.init_once(TaskQueue::new);
    }

    pub fn enqueue(task: Pin<Box<Task>>) {
        global_queue().enqueue(task);
    }

    #[must_use]
    pub fn dequeue() -> Option<Pin<Box<Task>>> {
        global_queue().dequeue()
    }
}
