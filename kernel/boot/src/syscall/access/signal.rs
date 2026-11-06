use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel_abi::{Errno, ProcessId, SigInfo};
use kernel_syscall::access::{
    Capability, Identity, PermissionAccess, ProcessAccess, ProcessesAccess, SignalAccess,
    SignalMaskAccess,
};
use kernel_syscall::signal::{Disposition, SignalMask};
use tracing::info;

use crate::mcore::mtask::process::tree::process_tree;
use crate::mcore::mtask::process::{ExitOutcome, Process};
use crate::mcore::mtask::task::Task;
use crate::syscall::access::KernelAccess;

/// Local wrapper so the foreign `ProcessAccess` trait can be implemented
/// for our process handles (orphan rule forbids `impl` for `Arc<Process>`).
pub struct KernelProcess(Arc<Process>);

impl ProcessAccess for KernelProcess {
    fn process_id(&self) -> ProcessId {
        self.0.pid()
    }

    fn process_group_id(&self) -> ProcessId {
        self.0.process_group_id()
    }
}

impl ProcessesAccess for KernelAccess {
    type Process = KernelProcess;

    fn all_processes(&self) -> impl Iterator<Item = Self::Process> {
        let processes: Vec<Arc<Process>> = process_tree().read().all().cloned().collect();
        processes.into_iter().map(KernelProcess)
    }
}

impl PermissionAccess for KernelAccess {
    fn current_identity(&self) -> Identity {
        // No uid or pgid exists on Process yet. Single user, every process
        // is its own group.
        Identity {
            process_id: self.process.pid(),
            user_id: 0,
            process_group_id: self.process.pid(),
        }
    }

    fn check_permission(&self, _target_pid: ProcessId, _cap: Capability) -> Result<(), Errno> {
        // Single-user kernel, everything is permitted.
        Ok(())
    }
}

impl SignalAccess for KernelAccess {
    fn deliver(&self, pid: ProcessId, info: SigInfo) {
        // A missing process races process death. sys_kill already
        // ESRCH-checked, so a silent no-op is correct here.
        let Some(process) = process_tree().read().processes.get(&pid).cloned() else {
            return;
        };
        let sender = Task::current();
        let sender_mask = (sender.process().pid() == pid).then(|| sender.signal_mask());
        let mut guard = process.signals_write();
        let effect = guard.deliver(info.signo);
        // Log stop and terminate outcomes at generation time. The victim may
        // consume the signal at a timer tick, and the timer handler must not
        // touch the serial lock, so this is the only safe place to record it.
        // A blocked default-terminate signal logs early while it stays
        // pending, which is acceptable for a single-user kernel.
        match guard.disposition(info.signo) {
            Disposition::DefaultStop => {
                info!("stopping process {pid} on signal {}", info.signo.name());
            }
            Disposition::DefaultTerminate => {
                info!(
                    "terminating process on signal {} (pid {pid})",
                    info.signo.name()
                );
                process.set_exit_outcome(ExitOutcome::Signaled(info.signo));
            }
            Disposition::Ignore | Disposition::Handler(_) => {}
        }
        if effect.must_resume_tasks {
            info!("continuing process {pid}");
            guard.resume_stopped_tasks();
        }
        process.wake_signal_taker(info.signo, sender_mask);
    }
}

impl SignalMaskAccess for KernelAccess {
    fn signal_mask(&self) -> SignalMask {
        Task::current().signal_mask()
    }

    fn set_signal_mask(&self, mask: SignalMask) {
        Task::current().set_signal_mask(mask);
    }
}
