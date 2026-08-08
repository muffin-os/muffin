use alloc::boxed::Box;
use core::pin::Pin;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::Relaxed;

use conquer_once::spin::OnceCell;

use crate::mcore::mtask::process::SignalsWriteGuard;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::{Task, TaskQueue};

static STOPPED_TASKS: OnceCell<TaskQueue> = OnceCell::uninit();

static PARKED: AtomicUsize = AtomicUsize::new(0);

fn stopped_tasks() -> &'static TaskQueue {
    STOPPED_TASKS.get().expect("StoppedTasks not initialized")
}

/// Parking lot for tasks whose process is stopped by a stop signal.
pub struct StoppedTasks;

impl StoppedTasks {
    pub fn init() {
        STOPPED_TASKS.init_once(TaskQueue::new);
    }

    /// Parks a task of a stopped process.
    pub fn park(task: Pin<Box<Task>>) {
        PARKED.fetch_add(1, Relaxed);
        stopped_tasks().enqueue(task);
    }

    /// Releases every parked task of the signalled process back to the run
    /// queue.
    pub fn resume_all(signals: &SignalsWriteGuard<'_>) {
        let pid = signals.pid();
        let queue = stopped_tasks();
        for _ in 0..PARKED.load(Relaxed) {
            let Some(task) = queue.dequeue() else {
                break;
            };
            if task.process().pid() == pid {
                PARKED.fetch_sub(1, Relaxed);
                GlobalTaskQueue::enqueue(task);
            } else {
                queue.enqueue(task);
            }
        }
    }
}
