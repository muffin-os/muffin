use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::pin::Pin;

use conquer_once::spin::OnceCell;
use kernel_abi::ProcessId;
use spin::Mutex;

use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::Task;

type ParkedTasks = BTreeMap<ProcessId, Vec<Pin<Box<Task>>>>;

static STOPPED_TASKS: OnceCell<Mutex<ParkedTasks>> = OnceCell::uninit();

fn stopped_tasks() -> &'static Mutex<ParkedTasks> {
    STOPPED_TASKS.get().expect("StoppedTasks not initialized")
}

/// Parking lot for tasks whose process is stopped by a stop signal.
///
/// Keyed by pid rather than owned by `Process` to avoid an Arc cycle
/// (`Process` owning a `Task` that owns `Arc<Process>`).
pub struct StoppedTasks;

impl StoppedTasks {
    pub fn init() {
        STOPPED_TASKS.init_once(|| Mutex::new(BTreeMap::new()));
    }

    /// Parks a task of a stopped process.
    ///
    /// # Errors
    /// Returns the task if the registry lock is contended.
    pub fn try_park(pid: ProcessId, task: Pin<Box<Task>>) -> Result<(), Pin<Box<Task>>> {
        let Some(mut guard) = stopped_tasks().try_lock() else {
            return Err(task);
        };
        guard.entry(pid).or_default().push(task);
        Ok(())
    }

    /// Re-enqueues all parked tasks of `pid`.
    ///
    /// Callers must hold the process's `signals` write guard (lock order:
    /// `Process.signals` before `StoppedTasks`), which serializes this
    /// against a concurrent park of the same process.
    pub fn resume_all(pid: ProcessId) {
        let mut guard = stopped_tasks().lock();
        if let Some(tasks) = guard.remove(&pid) {
            for task in tasks {
                GlobalTaskQueue::enqueue(task);
            }
        }
    }
}
