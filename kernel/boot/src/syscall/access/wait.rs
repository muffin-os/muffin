use core::time::Duration;

use kernel_abi::{EINTR, Errno, ProcessId};
use kernel_syscall::access::{ExitOutcome, WaitAccess};

use crate::hpet::hpet;
use crate::mcore::mtask::process::ParkOutcome;
use crate::mcore::mtask::process::tree::process_tree;
use crate::syscall::access::KernelAccess;

const POLL_INTERVAL: Duration = Duration::from_millis(1);

impl WaitAccess for KernelAccess {
    fn parent_id(&self, pid: ProcessId) -> Option<ProcessId> {
        process_tree().read().processes.get(&pid).map(|p| p.ppid())
    }

    fn exit_outcome(&self, pid: ProcessId) -> Option<ExitOutcome> {
        process_tree()
            .read()
            .processes
            .get(&pid)
            .and_then(|p| p.exit_outcome())
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
