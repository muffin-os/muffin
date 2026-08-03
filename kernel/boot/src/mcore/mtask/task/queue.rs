use alloc::boxed::Box;
use core::fmt::{Debug, Formatter};
use core::ops::Deref;

use cordyceps::MpscQueue;

use crate::mcore::mtask::task::Task;

pub struct TaskQueue {
    // Although this is a Mpsc, we can use it as Mpmc, because it spins if the queue
    // is currently being used by another thread. This is ok, because the spinning
    // is busy, and not by halting the CPU. (if you think that this might be incorrect,
    // double check obviously).
    inner: MpscQueue<Task>,
}

impl Default for TaskQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: MpscQueue::new_with_stub(Box::pin(Task::create_stub())),
        }
    }
}

impl Debug for TaskQueue {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TaskQueue").finish_non_exhaustive()
    }
}

impl Deref for TaskQueue {
    type Target = MpscQueue<Task>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
