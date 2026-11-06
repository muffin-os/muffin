use kernel_abi::{ECHILD, EINTR, Errno, ProcessId};
use kernel_syscall::access::WaitAccess;
use kernel_syscall::wait::{Reaped, WaitTarget};

use crate::mcore::mtask::process::tree::process_tree;
use crate::mcore::mtask::process::{ParkOutcome, Process};
use crate::mcore::mtask::task::Task;
use crate::syscall::access::KernelAccess;

impl WaitAccess for KernelAccess {
    fn parent_id(&self, pid: ProcessId) -> Option<ProcessId> {
        process_tree().read().processes.get(&pid).map(|p| p.ppid())
    }

    fn try_reap_child(
        &self,
        parent: ProcessId,
        caller_group: ProcessId,
        target: WaitTarget,
    ) -> Result<Option<Reaped>, Errno> {
        let accepts = |child: &Process| match target {
            WaitTarget::AnyChild => true,
            WaitTarget::Child(pid) => child.pid() == pid,
            WaitTarget::CallerGroup => child.process_group_id() == caller_group,
            WaitTarget::ProcessGroup(pgid) => child.process_group_id() == pgid,
        };
        let (removed, notify) = {
            let mut tree = process_tree().write();
            let mut has_match = false;
            let mut selected = None;
            for child in tree.all().filter(|p| p.ppid() == parent && accepts(p)) {
                has_match = true;
                if let Some(outcome) = child.exit_outcome() {
                    selected = Some(Reaped {
                        pid: child.pid(),
                        outcome,
                    });
                    break;
                }
            }
            if !has_match {
                return Err(ECHILD);
            }
            let Some(reaped) = selected else {
                return Ok(None);
            };
            let removed = tree.processes.remove(&reaped.pid);
            let notify = tree.processes.get(&parent).cloned();
            (removed.map(|process| (process, reaped)), notify)
        };
        // Competing waiters must check again, for example to observe `ECHILD`.
        if let Some(parent) = notify {
            parent.child_changes().wake_all();
        }
        Ok(removed.map(|(_process, reaped)| reaped))
    }

    fn wait_for_child_change(&self, mut should_wake: impl FnMut() -> bool) -> Result<(), Errno> {
        let task = Task::current();
        let mut is_complete = false;
        let outcome = self
            .process
            .park_current_task(None, &[self.process.child_changes()], || {
                is_complete = should_wake();
                is_complete || task.has_interrupting_signal()
            });
        if is_complete {
            return Ok(());
        }
        if matches!(outcome, ParkOutcome::Interrupted) || task.has_interrupting_signal() {
            return Err(EINTR);
        }
        Ok(())
    }
}
