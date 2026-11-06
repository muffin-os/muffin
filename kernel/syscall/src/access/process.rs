use kernel_abi::{Errno, ProcessId, Signal};

use crate::wait::{Reaped, WaitTarget};

pub trait ProcessesAccess {
    type Process: ProcessAccess;

    fn all_processes(&self) -> impl Iterator<Item = Self::Process>;

    fn process_by_id(&self, pid: ProcessId) -> Option<Self::Process> {
        self.all_processes().find(|p| p.process_id() == pid)
    }

    fn processes_in_group(
        &self,
        process_group_id: ProcessId,
    ) -> impl Iterator<Item = Self::Process> {
        self.all_processes()
            .filter(move |p| p.process_group_id() == process_group_id)
    }
}

pub trait ProcessAccess {
    fn process_id(&self) -> ProcessId;
    fn process_group_id(&self) -> ProcessId;
}

/// How a process ended. The first terminating event wins.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ExitOutcome {
    Exited(usize),
    Signaled(Signal),
}

impl ExitOutcome {
    /// The status `waitpid` reports: the exit code, or 128 plus the signal
    /// number for a signaled process.
    #[must_use]
    pub fn wait_status(self) -> i32 {
        match self {
            ExitOutcome::Exited(code) => code as i32,
            ExitOutcome::Signaled(signal) => 128 + signal.number(),
        }
    }
}

/// Process table access for `getppid` and `waitpid`.
pub trait WaitAccess {
    /// `None` when no process has `pid`.
    fn parent_id(&self, pid: ProcessId) -> Option<ProcessId>;

    /// Selects the child of `parent` with the lowest pid that matches `target` and has an exit
    /// outcome, and removes it from the process table in the same step.
    ///
    /// # Errors
    /// This function returns `ECHILD` when no child matches `target`.
    fn try_reap_child(
        &self,
        parent: ProcessId,
        caller_group: ProcessId,
        target: WaitTarget,
    ) -> Result<Option<Reaped>, Errno>;

    /// Parks the caller until a child change or a signal occurs. `should_wake` runs before the
    /// signal check.
    ///
    /// # Errors
    /// This function returns `EINTR` when a signal interrupts the wait and `should_wake` stays false.
    fn wait_for_child_change(&self, should_wake: impl FnMut() -> bool) -> Result<(), Errno>;
}
