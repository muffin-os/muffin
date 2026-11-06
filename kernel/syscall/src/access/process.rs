use kernel_abi::{ProcessId, Signal};

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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Child {
    pub pid: ProcessId,
    pub process_group_id: ProcessId,
    pub exit_outcome: Option<ExitOutcome>,
}

/// Process table lookups for `getppid` and `waitpid`.
pub trait WaitAccess {
    /// `None` when no process has `pid`.
    fn parent_id(&self, pid: ProcessId) -> Option<ProcessId>;
    /// Returns each child of `parent`, zombies included, in ascending pid order.
    fn children(&self, parent: ProcessId) -> impl Iterator<Item = Child>;
    /// Removes `pid` from the process table after a parent collected its
    /// status. This is not the task reap of exec.
    fn remove_process(&self, pid: ProcessId);
}
