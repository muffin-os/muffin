use alloc::vec::Vec;
use core::time::Duration;

use kernel_abi::{EINTR, Errno, ProcessId};
use kernel_syscall::access::{Child, WaitAccess};

use crate::hpet::hpet;
use crate::mcore::mtask::process::ParkOutcome;
use crate::mcore::mtask::process::tree::process_tree;
use crate::syscall::access::KernelAccess;

const POLL_INTERVAL: Duration = Duration::from_millis(1);

impl WaitAccess for KernelAccess {
    fn parent_id(&self, pid: ProcessId) -> Option<ProcessId> {
        process_tree().read().processes.get(&pid).map(|p| p.ppid())
    }

    fn children(&self, parent: ProcessId) -> impl Iterator<Item = Child> {
        let children: Vec<Child> = process_tree()
            .read()
            .all()
            .filter(|p| p.ppid() == parent)
            .map(|p| Child {
                pid: p.pid(),
                process_group_id: p.process_group_id(),
                exit_outcome: p.exit_outcome(),
            })
            .collect();
        children.into_iter()
    }

    fn remove_process(&self, pid: ProcessId) {
        process_tree().write().processes.remove(&pid);
    }

    fn sleep_one_poll(&self) -> Result<(), Errno> {
        let deadline = hpet().read().now().saturating_add(POLL_INTERVAL);
        let outcome = self.process.park_current_task(Some(deadline), || {
            hpet().read().now() >= deadline
                || self.process.signals_read().has_interrupting_deliverable()
        });
        if matches!(outcome, ParkOutcome::Interrupted)
            || self.process.signals_read().has_interrupting_deliverable()
        {
            return Err(EINTR);
        }
        Ok(())
    }
}
