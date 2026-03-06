use core::num::NonZeroI32;

use kernel_abi::{EINVAL, EPERM, ESRCH, Errno, NSIG_MAX, ProcessId, SigInfo, SigInfoField};

use crate::access::{
    Capability, Identity, PermissionAccess, ProcessAccess, ProcessesAccess, SignalAccess,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SignalTarget {
    BroadcastAll,
    SpecificProcess(ProcessId),
    ProcessGroup(ProcessId),
}

pub fn sys_kill<Cx: SignalAccess + PermissionAccess + ProcessesAccess>(
    cx: &Cx,
    target: SignalTarget,
    signal_number: NonZeroI32,
) -> Result<usize, Errno> {
    if signal_number.get() < 0 || signal_number.get() as usize > NSIG_MAX {
        return Err(EINVAL);
    }

    let Identity {
        process_id: current_pid,
        user_id: current_uid,
        process_group_id: current_pgid,
    } = cx.current_identity();

    let signal = SigInfo {
        signo: signal_number.get(),
        code: 0,
        errno: 0,
        info: SigInfoField::Kill {
            pid: current_pid,
            uid: current_uid,
        },
    };

    match target {
        SignalTarget::SpecificProcess(pid) => {
            let proc = cx.process_by_id(pid).ok_or(ESRCH)?;
            cx.check_permission(proc.process_id(), Capability::Signal)?;
            cx.deliver(pid, signal);
            Ok(0)
        }
        SignalTarget::BroadcastAll => distribute_signal(cx, cx.all_processes(), signal),
        SignalTarget::ProcessGroup(process_group_id) => {
            let effective_pgid = if process_group_id.is_root() {
                current_pgid
            } else {
                process_group_id
            };
            distribute_signal(cx, cx.processes_in_group(effective_pgid), signal)
        }
    }
}

fn distribute_signal<Cx, I>(cx: &Cx, iter: I, signal: SigInfo) -> Result<usize, Errno>
where
    Cx: SignalAccess + PermissionAccess + ProcessesAccess,
    I: Iterator<Item = <Cx as ProcessesAccess>::Process>,
{
    #[allow(clippy::manual_try_fold)] // doesn't provide any benefit in this case
    iter.fold(Err(ESRCH), |state, proc| {
        match cx.check_permission(proc.process_id(), Capability::Signal) {
            Ok(_) => {
                cx.deliver(proc.process_id(), signal);
                Ok(0)
            }
            Err(_) => {
                if state.is_ok() {
                    state
                } else {
                    Err(EPERM)
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::num::NonZeroI32;

    use kernel_abi::{
        EINVAL, EPERM, ESRCH, Errno, NSIG_MAX, ProcessId, SIGABRT, SIGALRM, SIGINT, SIGTERM,
        SigInfo, SigInfoField,
    };

    use crate::access::{
        Capability, Identity, PermissionAccess, ProcessAccess, ProcessesAccess, SignalAccess,
    };
    use crate::signal::{SignalTarget, sys_kill};

    macro_rules! pid {
        ($n:expr) => {
            ProcessId::from($n as u64)
        };
    }

    #[derive(Clone, Debug)]
    struct DeliveredSignal {
        pid: ProcessId,
        siginfo: SigInfo,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestProcess {
        pid: ProcessId,
        pgid: ProcessId,
        uid: u32,
    }

    impl ProcessAccess for TestProcess {
        fn process_id(&self) -> ProcessId {
            self.pid
        }

        fn process_group_id(&self) -> ProcessId {
            self.pgid
        }
    }

    struct TestContext {
        current: TestProcess,
        processes: Vec<TestProcess>,
        delivered: RefCell<Vec<DeliveredSignal>>,
        permission_denied: RefCell<Vec<ProcessId>>,
    }

    impl TestContext {
        fn new(current_pid: ProcessId, current_pgid: ProcessId, current_uid: u32) -> Self {
            let current = TestProcess {
                pid: current_pid,
                pgid: current_pgid,
                uid: current_uid,
            };
            Self {
                current,
                processes: vec![],
                delivered: RefCell::new(vec![]),
                permission_denied: RefCell::new(vec![]),
            }
        }

        fn add_process(&mut self, pid: ProcessId, pgid: ProcessId, uid: u32) {
            self.processes.push(TestProcess { pid, pgid, uid });
        }

        fn deny_permission(&self, pid: ProcessId) {
            self.permission_denied.borrow_mut().push(pid);
        }

        fn get_delivered(&self) -> Vec<DeliveredSignal> {
            self.delivered.borrow().clone()
        }
    }

    impl PermissionAccess for TestContext {
        fn current_identity(&self) -> Identity {
            Identity {
                process_id: self.current.pid,
                user_id: self.current.uid,
                process_group_id: self.current.pgid,
            }
        }

        fn check_permission(&self, target_pid: ProcessId, cap: Capability) -> Result<(), Errno> {
            assert_eq!(Capability::Signal, cap);

            if self.permission_denied.borrow().contains(&target_pid) {
                Err(EPERM)
            } else {
                Ok(())
            }
        }
    }

    impl ProcessesAccess for TestContext {
        type Process = TestProcess;

        fn all_processes(&self) -> impl Iterator<Item = Self::Process> {
            self.processes.clone().into_iter()
        }
    }

    impl SignalAccess for TestContext {
        fn deliver(&self, pid: ProcessId, info: SigInfo) {
            self.delivered
                .borrow_mut()
                .push(DeliveredSignal { pid, siginfo: info });
        }
    }

    #[test]
    fn test_signal_to_specific_process() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);
        let target_pgid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pgid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].pid, target_pid);
        assert_eq!(delivered[0].siginfo.signo, SIGINT);
    }

    #[test]
    fn test_signal_to_nonexistent_process() {
        let current_pid = pid!(1);
        let nonexistent_pid = pid!(999);

        let cx = TestContext::new(current_pid, current_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(nonexistent_pid),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(ESRCH));
    }

    #[test]
    fn test_signal_permission_denied() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1001);
        cx.deny_permission(target_pid);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(EPERM));
        assert!(cx.get_delivered().is_empty());
    }

    #[test]
    fn test_invalid_signal_number_negative() {
        let current_pid = pid!(1);
        let cx = TestContext::new(current_pid, current_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(current_pid),
            NonZeroI32::new(-1).unwrap(),
        );
        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn test_invalid_signal_number_exceeds_nsig() {
        let current_pid = pid!(1);
        let cx = TestContext::new(current_pid, current_pid, 1000);
        let invalid_signal = (NSIG_MAX + 1) as i32;

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(current_pid),
            NonZeroI32::new(invalid_signal).unwrap(),
        );
        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn test_valid_signal_number_boundary_min() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGABRT).unwrap(),
        );
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn test_valid_signal_number_boundary_max() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(NSIG_MAX as i32).unwrap(),
        );
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn test_siginfo_fields() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGTERM).unwrap(),
        )
        .unwrap();

        let delivered = cx.get_delivered();
        let siginfo = &delivered[0].siginfo;
        assert_eq!(siginfo.signo, SIGTERM);
        assert_eq!(siginfo.code, 0);
        assert_eq!(siginfo.errno, 0);
        match siginfo.info {
            SigInfoField::Kill { pid, uid } => {
                assert_eq!(pid, current_pid);
                assert_eq!(uid, 1000);
            }
            _ => panic!("Expected Kill variant"),
        }
    }

    #[test]
    fn test_broadcast_all_no_processes() {
        let current_pid = pid!(1);
        let cx = TestContext::new(current_pid, current_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(ESRCH));
    }

    #[test]
    fn test_broadcast_all_single_process() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].pid, target_pid);
    }

    #[test]
    fn test_broadcast_all_multiple_processes() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pid4 = pid!(4);
        let pgid2 = pid!(2);
        let pgid3 = pid!(3);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pgid2, 1000);
        cx.add_process(pid3, pgid2, 1000);
        cx.add_process(pid4, pgid3, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 3);
        let pids: Vec<_> = delivered.iter().map(|s| s.pid).collect();
        assert!(pids.contains(&pid2));
        assert!(pids.contains(&pid3));
        assert!(pids.contains(&pid4));
    }

    #[test]
    fn test_broadcast_all_permission_denied_returns_ok_if_any_delivered() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pgid2 = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pgid2, 1000);
        cx.add_process(pid3, pgid2, 1001);
        cx.deny_permission(pid3);

        let result = sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(
            result,
            Ok(0),
            "Should succeed if at least one signal delivered"
        );

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].pid, pid2);
    }

    #[test]
    fn test_broadcast_all_permission_denied_all_processes() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pgid2 = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pgid2, 1001);
        cx.add_process(pid3, pgid2, 1001);
        cx.deny_permission(pid2);
        cx.deny_permission(pid3);

        let result = sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(EPERM));
        assert!(cx.get_delivered().is_empty());
    }

    #[test]
    fn test_broadcast_all_preserves_success_after_failure() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pid4 = pid!(4);
        let pgid2 = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pgid2, 1000);
        cx.add_process(pid3, pgid2, 1001);
        cx.add_process(pid4, pgid2, 1000);
        cx.deny_permission(pid3);

        let result = sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(
            result,
            Ok(0),
            "Should return Ok if at least one signal was delivered"
        );

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2);
    }

    #[test]
    fn test_process_group_with_root_pid() {
        let current_pid = pid!(1);
        let current_pgid = pid!(5);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pid4 = pid!(4);
        let pgid5 = pid!(5);
        let pgid6 = pid!(6);

        let mut cx = TestContext::new(current_pid, current_pgid, 1000);
        cx.add_process(pid2, pgid5, 1000);
        cx.add_process(pid3, pgid5, 1000);
        cx.add_process(pid4, pgid6, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::ProcessGroup(pid!(0)),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2);
        let pids: Vec<_> = delivered.iter().map(|s| s.pid).collect();
        assert!(pids.contains(&pid2));
        assert!(pids.contains(&pid3));
    }

    #[test]
    fn test_process_group_specific_pgid() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pid4 = pid!(4);
        let pgid5 = pid!(5);
        let pgid6 = pid!(6);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pgid5, 1000);
        cx.add_process(pid3, pgid5, 1000);
        cx.add_process(pid4, pgid6, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::ProcessGroup(pgid5),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2);
        let pids: Vec<_> = delivered.iter().map(|s| s.pid).collect();
        assert!(pids.contains(&pid2));
        assert!(pids.contains(&pid3));
        assert!(!pids.contains(&pid4));
    }

    #[test]
    fn test_process_group_no_matching_processes() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let target_pgid = pid!(5);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pid2, 1000);
        cx.add_process(pid3, pid3, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::ProcessGroup(target_pgid),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(ESRCH));
        assert!(cx.get_delivered().is_empty());
    }

    #[test]
    fn test_process_group_permission_denied() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pgid5 = pid!(5);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pgid5, 1001);
        cx.add_process(pid3, pgid5, 1001);
        cx.deny_permission(pid2);
        cx.deny_permission(pid3);

        let result = sys_kill(
            &cx,
            SignalTarget::ProcessGroup(pgid5),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(EPERM));
        assert!(cx.get_delivered().is_empty());
    }

    #[test]
    fn test_process_group_mixed_permissions() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pgid5 = pid!(5);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pgid5, 1000);
        cx.add_process(pid3, pgid5, 1001);
        cx.deny_permission(pid3);

        let result = sys_kill(
            &cx,
            SignalTarget::ProcessGroup(pgid5),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].pid, pid2);
    }

    #[test]
    fn test_signal_to_self() {
        let current_pid = pid!(1);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(current_pid, current_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(current_pid),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].pid, current_pid);
    }

    #[test]
    fn test_signal_number_sigkill() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));
        assert_eq!(cx.get_delivered()[0].siginfo.signo, SIGINT);
    }

    #[test]
    fn test_signal_number_sigterm() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGTERM).unwrap(),
        );
        assert_eq!(result, Ok(0));
        assert_eq!(cx.get_delivered()[0].siginfo.signo, SIGTERM);
    }

    #[test]
    fn test_signal_number_sighup() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGABRT).unwrap(),
        );
        assert_eq!(result, Ok(0));
        assert_eq!(cx.get_delivered()[0].siginfo.signo, SIGABRT);
    }

    #[test]
    fn test_signal_number_sigint() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGALRM).unwrap(),
        );
        assert_eq!(result, Ok(0));
        assert_eq!(cx.get_delivered()[0].siginfo.signo, SIGALRM);
    }

    #[test]
    fn test_multiple_signals_to_same_process() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGABRT).unwrap(),
        )
        .unwrap();
        sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(SIGALRM).unwrap(),
        )
        .unwrap();

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[0].pid, target_pid);
        assert_eq!(delivered[1].pid, target_pid);
        assert_eq!(delivered[0].siginfo.signo, SIGABRT);
        assert_eq!(delivered[1].siginfo.signo, SIGALRM);
    }

    #[test]
    fn test_broadcast_respects_current_identity() {
        let current_pid = pid!(5);
        let current_pgid = pid!(10);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pgid2 = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pgid, 2000);
        cx.add_process(pid2, pgid2, 1000);
        cx.add_process(pid3, pgid2, 1000);

        sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        )
        .unwrap();

        let delivered = cx.get_delivered();
        for signal in delivered {
            match signal.siginfo.info {
                SigInfoField::Kill { pid, uid } => {
                    assert_eq!(pid, current_pid);
                    assert_eq!(uid, 2000);
                }
                _ => panic!("Expected Kill variant"),
            }
        }
    }

    #[test]
    fn test_process_group_zero_uses_current_pgid() {
        let current_pid = pid!(7);
        let pid8 = pid!(8);
        let pid9 = pid!(9);
        let pid10 = pid!(10);
        let pgid7 = pid!(7);
        let pgid8 = pid!(8);

        let mut cx = TestContext::new(current_pid, pgid7, 1000);
        cx.add_process(pid8, pgid7, 1000);
        cx.add_process(pid9, pgid7, 1000);
        cx.add_process(pid10, pgid8, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::ProcessGroup(pid!(0)),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2);
        let pids: Vec<_> = delivered.iter().map(|s| s.pid).collect();
        assert!(pids.contains(&pid8));
        assert!(pids.contains(&pid9));
        assert!(!pids.contains(&pid10)); // pgid mismatch
    }

    #[test]
    fn test_large_process_group() {
        let current_pid = pid!(1);
        let pgid5 = pid!(5);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        for i in 2..102 {
            cx.add_process(pid!(i), pgid5, 1000);
        }

        let result = sys_kill(
            &cx,
            SignalTarget::ProcessGroup(pgid5),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 100);
    }

    #[test]
    fn test_permission_check_with_different_uids() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pid2, 1001);
        cx.add_process(pid3, pid3, 1002);
        cx.deny_permission(pid2);
        cx.deny_permission(pid3);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(pid2),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(EPERM));

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(pid3),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(EPERM));
    }

    #[test]
    fn test_distribute_signal_order_independence() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);
        let pid3 = pid!(3);
        let pid4 = pid!(4);
        let pgid5 = pid!(5);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pgid5, 1000);
        cx.add_process(pid3, pgid5, 1001);
        cx.add_process(pid4, pgid5, 1000);
        cx.deny_permission(pid3);

        let result = sys_kill(
            &cx,
            SignalTarget::ProcessGroup(pgid5),
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Ok(0));

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2);
    }

    #[test]
    fn test_empty_process_list_broadcast() {
        let current_pid = pid!(1);
        let cx = TestContext::new(current_pid, current_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(ESRCH));
        assert!(cx.get_delivered().is_empty());
    }

    #[test]
    fn test_single_permission_failure_in_broadcast() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pid2, 1001);
        cx.deny_permission(pid2);

        let result = sys_kill(
            &cx,
            SignalTarget::BroadcastAll,
            NonZeroI32::new(SIGINT).unwrap(),
        );
        assert_eq!(result, Err(EPERM));
        assert!(cx.get_delivered().is_empty());
    }

    #[test]
    fn test_edge_case_nsig_boundary() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new((NSIG_MAX + 1) as i32).unwrap(),
        );
        assert_eq!(result, Err(EINVAL));

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            NonZeroI32::new(NSIG_MAX as i32).unwrap(),
        );
        assert_eq!(result, Ok(0));
    }
}
