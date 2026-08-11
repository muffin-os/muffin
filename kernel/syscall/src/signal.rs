use kernel_abi::{
    DefaultAction, EINVAL, EPERM, ESRCH, Errno, ProcessId, STOP_SIGNALS_MASK, SaFlags, SigAction,
    SigInfo, SigInfoField, SigMaskHow, SigSet, Signal, default_action,
};
use tracing::{Level, instrument};

use crate::access::{
    Capability, Identity, PermissionAccess, ProcessAccess, ProcessesAccess, SignalAccess,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SignalTarget {
    BroadcastAll,
    SpecificProcess(ProcessId),
    ProcessGroup(ProcessId),
}

#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_kill<Cx: SignalAccess + PermissionAccess + ProcessesAccess>(
    cx: &Cx,
    target: SignalTarget,
    signal: Signal,
) -> Result<usize, Errno> {
    let Identity {
        process_id: current_pid,
        user_id: current_uid,
        process_group_id: current_pgid,
    } = cx.current_identity();

    let info = SigInfo {
        signo: signal,
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
            cx.deliver(pid, info);
            Ok(0)
        }
        SignalTarget::BroadcastAll => distribute_signal(cx, cx.all_processes(), info),
        SignalTarget::ProcessGroup(process_group_id) => {
            let effective_pgid = if process_group_id.is_root() {
                current_pgid
            } else {
                process_group_id
            };
            distribute_signal(cx, cx.processes_in_group(effective_pgid), info)
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

/// Disposition of a signal for the owning process, resolved against the
/// installed action and the default action table.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Disposition {
    Ignore,
    DefaultTerminate,
    DefaultStop,
    Handler(SigAction),
}

/// Side effect the kernel must apply after a generation-time `deliver`.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct DeliverEffect {
    pub resume_tasks: bool,
}

/// Per-process signal state. One instance lives behind a lock on every
/// `Process`. All delivery policy is decided here so it can be host-tested.
#[derive(Debug)]
pub struct SignalState {
    actions: [SigAction; Signal::COUNT],
    pending: SigSet,
    blocked: SigSet,
    stopped: bool,
}

impl Default for SignalState {
    fn default() -> Self {
        Self {
            actions: [SigAction::default(); Signal::COUNT],
            pending: 0,
            blocked: 0,
            stopped: false,
        }
    }
}

const KILL_STOP_MASK: SigSet = Signal::Kill.bit() | Signal::Stop.bit();

impl SignalState {
    const fn index(signo: Signal) -> usize {
        (signo.number() - 1) as usize
    }

    fn action(&self, signo: Signal) -> SigAction {
        self.actions[Self::index(signo)]
    }

    fn signals_in(mask: SigSet) -> impl Iterator<Item = Signal> {
        (0..Signal::COUNT as i32).filter_map(move |i| {
            if mask & (1 << i) != 0 {
                Signal::try_from(i + 1).ok()
            } else {
                None
            }
        })
    }

    /// Generation-time delivery. Records the signal as pending and, for a
    /// stopped process receiving `Cont` or `Kill`, clears the stopped flag so
    /// the task can resume (and, for `Kill`, die at its next safe point).
    pub fn deliver(&mut self, signo: Signal) -> DeliverEffect {
        self.set_pending(signo);
        if matches!(signo, Signal::Continue | Signal::Kill) && self.stopped {
            self.stopped = false;
            DeliverEffect { resume_tasks: true }
        } else {
            DeliverEffect {
                resume_tasks: false,
            }
        }
    }

    /// Set the pending bit, enforcing the POSIX stop/continue mutual discard.
    /// A stop-family signal drops any pending `Cont`, and `Cont` drops any
    /// pending stop-family signal.
    pub fn set_pending(&mut self, signo: Signal) {
        self.pending |= signo.bit();
        if STOP_SIGNALS_MASK & signo.bit() != 0 {
            self.pending &= !Signal::Continue.bit();
        } else if signo == Signal::Continue {
            self.pending &= !STOP_SIGNALS_MASK;
        }
    }

    /// Take the lowest-numbered deliverable (pending and unblocked) signal,
    /// clearing its pending bit.
    #[must_use]
    pub fn take_next_deliverable(&mut self) -> Option<Signal> {
        let deliverable = self.pending & !self.blocked;
        if deliverable == 0 {
            return None;
        }
        let signo = Signal::try_from(deliverable.trailing_zeros() as i32 + 1).ok()?;
        self.pending &= !signo.bit();
        Some(signo)
    }

    /// Whether any deliverable signal would terminate the process by default.
    #[must_use]
    pub fn has_fatal_deliverable(&self) -> bool {
        let deliverable = self.pending & !self.blocked;
        Self::signals_in(deliverable)
            .any(|s| matches!(self.disposition(s), Disposition::DefaultTerminate))
    }

    /// Whether any deliverable signal should abort a blocked kernel wait with
    /// `EINTR`. This is the predicate an interruptible wait re-checks after
    /// being woken.
    ///
    /// Ignored signals must not cause `EINTR`, and a stop leaves the sleeper
    /// parked, so only a handler or a default-terminate disposition counts.
    #[must_use]
    pub fn has_interrupting_deliverable(&self) -> bool {
        let deliverable = self.pending & !self.blocked;
        Self::signals_in(deliverable).any(|s| {
            matches!(
                self.disposition(s),
                Disposition::Handler(_) | Disposition::DefaultTerminate
            )
        })
    }

    /// Resolve the disposition of a signal. `Kill` is always terminate, its
    /// action can never be changed.
    #[must_use]
    pub fn disposition(&self, signo: Signal) -> Disposition {
        if signo == Signal::Kill {
            return Disposition::DefaultTerminate;
        }
        let action = self.action(signo);
        if action.handler.is_ignore() {
            Disposition::Ignore
        } else if action.handler.is_default() {
            match default_action(signo) {
                DefaultAction::Ignore => Disposition::Ignore,
                DefaultAction::Terminate => Disposition::DefaultTerminate,
                DefaultAction::Stop => Disposition::DefaultStop,
            }
        } else {
            Disposition::Handler(action)
        }
    }

    /// Install a new action (when `new.is_some()`) and return the previous one.
    /// Rejects changing `Kill`/`Stop`, custom handlers without a restorer, and
    /// the unsupported `SIGINFO` flag.
    pub fn sigaction(&mut self, signo: Signal, new: Option<SigAction>) -> Result<SigAction, Errno> {
        let old = self.action(signo);
        if let Some(new) = new {
            if matches!(signo, Signal::Kill | Signal::Stop) {
                return Err(EINVAL);
            }
            let is_custom = !new.handler.is_default() && !new.handler.is_ignore();
            if is_custom && new.restorer == 0 {
                return Err(EINVAL);
            }
            if new.flags.contains(SaFlags::SIGINFO) {
                return Err(EINVAL);
            }
            self.actions[Self::index(signo)] = new;
        }
        Ok(old)
    }

    /// Apply a mask change and return the previous mask. `Kill`/`Stop` can
    /// never be blocked, so their bits are always cleared afterwards.
    pub fn sigprocmask(&mut self, how: SigMaskHow, set: Option<SigSet>) -> Result<SigSet, Errno> {
        let old = self.blocked;
        if let Some(set) = set {
            match how {
                SigMaskHow::Block => self.blocked |= set,
                SigMaskHow::Unblock => self.blocked &= !set,
                SigMaskHow::SetMask => self.blocked = set,
            }
            self.blocked &= !KILL_STOP_MASK;
        }
        Ok(old)
    }

    #[must_use]
    pub fn sigpending(&self) -> SigSet {
        self.pending
    }

    #[must_use]
    pub fn blocked(&self) -> SigSet {
        self.blocked
    }

    /// Restore the blocked mask (used by sigreturn), always dropping the
    /// unblockable `Kill`/`Stop` bits.
    pub fn set_blocked_raw(&mut self, set: SigSet) {
        self.blocked = set & !KILL_STOP_MASK;
    }

    /// Apply the mask changes that accompany entering a handler and return the
    /// old mask. Blocks `action.mask` plus the delivered signal (unless
    /// `NODEFER`), keeps `Kill`/`Stop` unblockable, and resets the action to
    /// default when `RESETHAND` is set.
    pub fn apply_handler_entry(&mut self, signo: Signal, action: &SigAction) -> SigSet {
        let old_blocked = self.blocked;
        self.blocked |= action.mask;
        if !action.flags.contains(SaFlags::NODEFER) {
            self.blocked |= signo.bit();
        }
        self.blocked &= !KILL_STOP_MASK;
        if action.flags.contains(SaFlags::RESETHAND) {
            self.actions[Self::index(signo)] = SigAction::default();
        }
        old_blocked
    }

    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn set_stopped(&mut self, stopped: bool) {
        self.stopped = stopped;
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use kernel_abi::{
        EINVAL, EPERM, ESRCH, Errno, ProcessId, STOP_SIGNALS_MASK, SaFlags, SigAction, SigHandler,
        SigInfo, SigInfoField, SigMaskHow, Signal,
    };

    use crate::access::{
        Capability, Identity, PermissionAccess, ProcessAccess, ProcessesAccess, SignalAccess,
    };
    use crate::signal::{Disposition, SignalState, SignalTarget, sys_kill};

    macro_rules! pid {
        ($n:expr) => {
            ProcessId::from($n as u64)
        };
    }

    fn custom_action(addr: usize) -> SigAction {
        SigAction {
            handler: SigHandler::new(addr),
            mask: 0,
            flags: SaFlags::default(),
            restorer: 1,
        }
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
            assert_eq!(Capability::Signal, cap, "unexpected capability checked");

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
            Signal::Interrupt,
        );
        assert_eq!(result, Ok(0), "kill to specific process succeeds");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1, "one signal delivered");
        assert_eq!(delivered[0].pid, target_pid, "delivered to target");
        assert_eq!(
            delivered[0].siginfo.signo,
            Signal::Interrupt,
            "delivered SIGINT"
        );
    }

    #[test]
    fn test_signal_to_nonexistent_process() {
        let current_pid = pid!(1);
        let nonexistent_pid = pid!(999);

        let cx = TestContext::new(current_pid, current_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(nonexistent_pid),
            Signal::Interrupt,
        );
        assert_eq!(result, Err(ESRCH), "nonexistent target yields ESRCH");
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
            Signal::Interrupt,
        );
        assert_eq!(result, Err(EPERM), "denied permission yields EPERM");
        assert!(cx.get_delivered().is_empty(), "nothing delivered on deny");
    }

    #[test]
    fn test_invalid_signal_number_negative() {
        assert_eq!(
            Signal::try_from(-1),
            Err(EINVAL),
            "negative number is not a signal"
        );
        assert_eq!(Signal::try_from(0), Err(EINVAL), "zero is not a signal");
    }

    #[test]
    fn test_invalid_signal_number_exceeds_nsig() {
        assert_eq!(Signal::try_from(28), Err(EINVAL), "28 is out of range");
        assert_eq!(Signal::try_from(64), Err(EINVAL), "64 is out of range");
        assert_eq!(Signal::try_from(65), Err(EINVAL), "65 is out of range");
    }

    #[test]
    fn test_valid_signal_number_boundary_min() {
        assert_eq!(
            Signal::try_from(1),
            Ok(Signal::Abort),
            "1 maps to the first signal"
        );

        let current_pid = pid!(1);
        let target_pid = pid!(2);
        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            Signal::Abort,
        );
        assert_eq!(result, Ok(0), "kill with min signal succeeds");
    }

    #[test]
    fn test_valid_signal_number_boundary_max() {
        assert_eq!(
            Signal::try_from(Signal::COUNT as i32),
            Ok(Signal::ExceededFileSizeLimit),
            "COUNT maps to the last signal"
        );

        let current_pid = pid!(1);
        let target_pid = pid!(2);
        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            Signal::ExceededFileSizeLimit,
        );
        assert_eq!(result, Ok(0), "kill with max signal succeeds");
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
            Signal::Terminate,
        )
        .unwrap();

        let delivered = cx.get_delivered();
        let siginfo = &delivered[0].siginfo;
        assert_eq!(siginfo.signo, Signal::Terminate, "signo is SIGTERM");
        assert_eq!(siginfo.code, 0, "code is zero");
        assert_eq!(siginfo.errno, 0, "errno is zero");
        match siginfo.info {
            SigInfoField::Kill { pid, uid } => {
                assert_eq!(pid, current_pid, "sender pid recorded");
                assert_eq!(uid, 1000, "sender uid recorded");
            }
            _ => panic!("Expected Kill variant"),
        }
    }

    #[test]
    fn test_broadcast_all_no_processes() {
        let current_pid = pid!(1);
        let cx = TestContext::new(current_pid, current_pid, 1000);

        let result = sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt);
        assert_eq!(result, Err(ESRCH), "broadcast with no targets yields ESRCH");
    }

    #[test]
    fn test_broadcast_all_single_process() {
        let current_pid = pid!(1);
        let target_pid = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(target_pid, target_pid, 1000);

        let result = sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt);
        assert_eq!(result, Ok(0), "broadcast to one target succeeds");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1, "one signal delivered");
        assert_eq!(delivered[0].pid, target_pid, "delivered to the target");
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

        let result = sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt);
        assert_eq!(result, Ok(0), "broadcast to many succeeds");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 3, "all three targets received");
        let pids: Vec<_> = delivered.iter().map(|s| s.pid).collect();
        assert!(pids.contains(&pid2), "pid2 received");
        assert!(pids.contains(&pid3), "pid3 received");
        assert!(pids.contains(&pid4), "pid4 received");
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

        let result = sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt);
        assert_eq!(
            result,
            Ok(0),
            "Should succeed if at least one signal delivered"
        );

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1, "only the permitted target received");
        assert_eq!(delivered[0].pid, pid2, "delivered to pid2");
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

        let result = sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt);
        assert_eq!(result, Err(EPERM), "all denied yields EPERM");
        assert!(cx.get_delivered().is_empty(), "nothing delivered");
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

        let result = sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt);
        assert_eq!(
            result,
            Ok(0),
            "Should return Ok if at least one signal was delivered"
        );

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2, "two permitted targets received");
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

        let result = sys_kill(&cx, SignalTarget::ProcessGroup(pid!(0)), Signal::Interrupt);
        assert_eq!(result, Ok(0), "group kill via root pid succeeds");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2, "two group members received");
        let pids: Vec<_> = delivered.iter().map(|s| s.pid).collect();
        assert!(pids.contains(&pid2), "pid2 in group received");
        assert!(pids.contains(&pid3), "pid3 in group received");
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

        let result = sys_kill(&cx, SignalTarget::ProcessGroup(pgid5), Signal::Interrupt);
        assert_eq!(result, Ok(0), "specific group kill succeeds");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2, "two group members received");
        let pids: Vec<_> = delivered.iter().map(|s| s.pid).collect();
        assert!(pids.contains(&pid2), "pid2 received");
        assert!(pids.contains(&pid3), "pid3 received");
        assert!(!pids.contains(&pid4), "pid4 in other group untouched");
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
            Signal::Interrupt,
        );
        assert_eq!(result, Err(ESRCH), "empty group yields ESRCH");
        assert!(cx.get_delivered().is_empty(), "nothing delivered");
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

        let result = sys_kill(&cx, SignalTarget::ProcessGroup(pgid5), Signal::Interrupt);
        assert_eq!(result, Err(EPERM), "group all denied yields EPERM");
        assert!(cx.get_delivered().is_empty(), "nothing delivered");
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

        let result = sys_kill(&cx, SignalTarget::ProcessGroup(pgid5), Signal::Interrupt);
        assert_eq!(result, Ok(0), "partial permission still succeeds");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1, "only permitted member received");
        assert_eq!(delivered[0].pid, pid2, "pid2 received");
    }

    #[test]
    fn test_signal_to_self() {
        let current_pid = pid!(1);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(current_pid, current_pid, 1000);

        let result = sys_kill(
            &cx,
            SignalTarget::SpecificProcess(current_pid),
            Signal::Interrupt,
        );
        assert_eq!(result, Ok(0), "self kill succeeds");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 1, "one signal delivered");
        assert_eq!(delivered[0].pid, current_pid, "delivered to self");
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
            Signal::Interrupt,
        );
        assert_eq!(result, Ok(0), "kill succeeds");
        assert_eq!(
            cx.get_delivered()[0].siginfo.signo,
            Signal::Interrupt,
            "delivered SIGINT"
        );
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
            Signal::Terminate,
        );
        assert_eq!(result, Ok(0), "kill succeeds");
        assert_eq!(
            cx.get_delivered()[0].siginfo.signo,
            Signal::Terminate,
            "delivered SIGTERM"
        );
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
            Signal::Abort,
        );
        assert_eq!(result, Ok(0), "kill succeeds");
        assert_eq!(
            cx.get_delivered()[0].siginfo.signo,
            Signal::Abort,
            "delivered SIGABRT"
        );
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
            Signal::Alarm,
        );
        assert_eq!(result, Ok(0), "kill succeeds");
        assert_eq!(
            cx.get_delivered()[0].siginfo.signo,
            Signal::Alarm,
            "delivered SIGALRM"
        );
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
            Signal::Abort,
        )
        .unwrap();
        sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target_pid),
            Signal::Alarm,
        )
        .unwrap();

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2, "both signals delivered");
        assert_eq!(delivered[0].pid, target_pid, "first to target");
        assert_eq!(delivered[1].pid, target_pid, "second to target");
        assert_eq!(
            delivered[0].siginfo.signo,
            Signal::Abort,
            "first is SIGABRT"
        );
        assert_eq!(
            delivered[1].siginfo.signo,
            Signal::Alarm,
            "second is SIGALRM"
        );
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

        sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt).unwrap();

        let delivered = cx.get_delivered();
        for signal in delivered {
            match signal.siginfo.info {
                SigInfoField::Kill { pid, uid } => {
                    assert_eq!(pid, current_pid, "sender pid is caller");
                    assert_eq!(uid, 2000, "sender uid is caller");
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

        let result = sys_kill(&cx, SignalTarget::ProcessGroup(pid!(0)), Signal::Interrupt);
        assert_eq!(result, Ok(0), "group zero uses caller pgid");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2, "two caller-group members received");
        let pids: Vec<_> = delivered.iter().map(|s| s.pid).collect();
        assert!(pids.contains(&pid8), "pid8 received");
        assert!(pids.contains(&pid9), "pid9 received");
        assert!(!pids.contains(&pid10), "pid10 in other group untouched");
    }

    #[test]
    fn test_large_process_group() {
        let current_pid = pid!(1);
        let pgid5 = pid!(5);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        for i in 2..102 {
            cx.add_process(pid!(i), pgid5, 1000);
        }

        let result = sys_kill(&cx, SignalTarget::ProcessGroup(pgid5), Signal::Interrupt);
        assert_eq!(result, Ok(0), "large group kill succeeds");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 100, "all group members received");
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

        let result = sys_kill(&cx, SignalTarget::SpecificProcess(pid2), Signal::Interrupt);
        assert_eq!(result, Err(EPERM), "denied pid2 yields EPERM");

        let result = sys_kill(&cx, SignalTarget::SpecificProcess(pid3), Signal::Interrupt);
        assert_eq!(result, Err(EPERM), "denied pid3 yields EPERM");
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

        let result = sys_kill(&cx, SignalTarget::ProcessGroup(pgid5), Signal::Interrupt);
        assert_eq!(result, Ok(0), "mixed permissions still succeed");

        let delivered = cx.get_delivered();
        assert_eq!(delivered.len(), 2, "two permitted members received");
    }

    #[test]
    fn test_empty_process_list_broadcast() {
        let current_pid = pid!(1);
        let cx = TestContext::new(current_pid, current_pid, 1000);

        let result = sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt);
        assert_eq!(result, Err(ESRCH), "empty broadcast yields ESRCH");
        assert!(cx.get_delivered().is_empty(), "nothing delivered");
    }

    #[test]
    fn test_single_permission_failure_in_broadcast() {
        let current_pid = pid!(1);
        let pid2 = pid!(2);

        let mut cx = TestContext::new(current_pid, current_pid, 1000);
        cx.add_process(pid2, pid2, 1001);
        cx.deny_permission(pid2);

        let result = sys_kill(&cx, SignalTarget::BroadcastAll, Signal::Interrupt);
        assert_eq!(result, Err(EPERM), "sole denied target yields EPERM");
        assert!(cx.get_delivered().is_empty(), "nothing delivered");
    }

    #[test]
    fn test_edge_case_nsig_boundary() {
        assert_eq!(
            Signal::try_from(Signal::COUNT as i32 + 1),
            Err(EINVAL),
            "one past the last signal is invalid"
        );
        assert_eq!(
            Signal::try_from(Signal::COUNT as i32),
            Ok(Signal::ExceededFileSizeLimit),
            "the last signal is valid"
        );
    }

    #[test]
    fn sigaction_rejects_sigkill() {
        let mut state = SignalState::default();
        assert_eq!(
            state.sigaction(Signal::Kill, Some(custom_action(0x1000))),
            Err(EINVAL),
            "SIGKILL action cannot be changed"
        );
    }

    #[test]
    fn sigaction_rejects_sigstop() {
        let mut state = SignalState::default();
        assert_eq!(
            state.sigaction(Signal::Stop, Some(custom_action(0x1000))),
            Err(EINVAL),
            "SIGSTOP action cannot be changed"
        );
    }

    #[test]
    fn sigaction_rejects_zero_restorer() {
        let mut state = SignalState::default();
        let mut action = custom_action(0x1000);
        action.restorer = 0;
        assert_eq!(
            state.sigaction(Signal::Interrupt, Some(action)),
            Err(EINVAL),
            "custom handler without a restorer is rejected"
        );
    }

    #[test]
    fn sigaction_rejects_siginfo() {
        let mut state = SignalState::default();
        let mut action = custom_action(0x1000);
        action.flags = SaFlags::SIGINFO;
        assert_eq!(
            state.sigaction(Signal::Interrupt, Some(action)),
            Err(EINVAL),
            "SA_SIGINFO is unsupported"
        );
    }

    #[test]
    fn sigaction_allows_tstp_handler() {
        let mut state = SignalState::default();
        let action = custom_action(0x2000);
        let previous = state.sigaction(Signal::TerminalStop, Some(action));
        assert_eq!(
            previous,
            Ok(SigAction::default()),
            "returns the previous default action"
        );
        assert_eq!(
            state.disposition(Signal::TerminalStop),
            Disposition::Handler(action),
            "SIGTSTP handler is installed and catchable"
        );
    }

    #[test]
    fn sigaction_returns_previous_action() {
        let mut state = SignalState::default();
        let first = custom_action(0x1000);
        state.sigaction(Signal::Usr1, Some(first)).unwrap();
        let previous = state.sigaction(Signal::Usr1, Some(custom_action(0x2000)));
        assert_eq!(
            previous,
            Ok(first),
            "second install returns the first action"
        );
    }

    #[test]
    fn sigprocmask_never_blocks_kill_stop() {
        let mut state = SignalState::default();
        let full = u64::MAX;
        state.sigprocmask(SigMaskHow::Block, Some(full)).unwrap();
        assert_eq!(
            state.blocked() & Signal::Kill.bit(),
            0,
            "SIGKILL can never be blocked"
        );
        assert_eq!(
            state.blocked() & Signal::Stop.bit(),
            0,
            "SIGSTOP can never be blocked"
        );
    }

    #[test]
    fn sigprocmask_applies_modes_and_returns_previous() {
        let mut state = SignalState::default();
        let prev = state
            .sigprocmask(SigMaskHow::Block, Some(Signal::Interrupt.bit()))
            .unwrap();
        assert_eq!(prev, 0, "previous mask was empty");
        assert_ne!(
            state.blocked() & Signal::Interrupt.bit(),
            0,
            "SIGINT now blocked"
        );

        state
            .sigprocmask(SigMaskHow::Unblock, Some(Signal::Interrupt.bit()))
            .unwrap();
        assert_eq!(
            state.blocked() & Signal::Interrupt.bit(),
            0,
            "SIGINT unblocked"
        );

        state
            .sigprocmask(SigMaskHow::SetMask, Some(Signal::Terminate.bit()))
            .unwrap();
        assert_eq!(
            state.blocked(),
            Signal::Terminate.bit(),
            "SetMask replaces the whole mask"
        );

        let query = state.sigprocmask(SigMaskHow::Block, None).unwrap();
        assert_eq!(
            query,
            Signal::Terminate.bit(),
            "None queries without mutating"
        );
        assert_eq!(
            state.blocked(),
            Signal::Terminate.bit(),
            "query left mask intact"
        );
    }

    #[test]
    fn take_next_deliverable_lowest_first() {
        let mut state = SignalState::default();
        state.set_pending(Signal::Terminate);
        state.set_pending(Signal::Interrupt);
        assert_eq!(
            state.take_next_deliverable(),
            Some(Signal::Interrupt),
            "lowest-numbered signal delivered first"
        );
        assert_eq!(
            state.take_next_deliverable(),
            Some(Signal::Terminate),
            "next-lowest delivered second"
        );
        assert_eq!(state.take_next_deliverable(), None, "nothing left");
    }

    #[test]
    fn take_next_deliverable_honors_blocked() {
        let mut state = SignalState::default();
        state
            .sigprocmask(SigMaskHow::Block, Some(Signal::Interrupt.bit()))
            .unwrap();
        state.set_pending(Signal::Interrupt);
        assert_eq!(
            state.take_next_deliverable(),
            None,
            "blocked signal is not deliverable"
        );
        state
            .sigprocmask(SigMaskHow::Unblock, Some(Signal::Interrupt.bit()))
            .unwrap();
        assert_eq!(
            state.take_next_deliverable(),
            Some(Signal::Interrupt),
            "unblocking makes it deliverable"
        );
    }

    #[test]
    fn take_next_deliverable_coalesces() {
        let mut state = SignalState::default();
        state.set_pending(Signal::Interrupt);
        state.set_pending(Signal::Interrupt);
        assert_eq!(
            state.take_next_deliverable(),
            Some(Signal::Interrupt),
            "repeated pending signal delivered once"
        );
        assert_eq!(state.take_next_deliverable(), None, "no second copy queued");
    }

    #[test]
    fn apply_handler_entry_nodefer_skips_signo() {
        let mut state = SignalState::default();
        let mut action = custom_action(0x1000);
        action.flags = SaFlags::NODEFER;
        let old = state.apply_handler_entry(Signal::Interrupt, &action);
        assert_eq!(old, 0, "old mask was empty");
        assert_eq!(
            state.blocked() & Signal::Interrupt.bit(),
            0,
            "NODEFER leaves the delivered signal unblocked"
        );
    }

    #[test]
    fn apply_handler_entry_blocks_signo_by_default() {
        let mut state = SignalState::default();
        let action = custom_action(0x1000);
        state.apply_handler_entry(Signal::Interrupt, &action);
        assert_ne!(
            state.blocked() & Signal::Interrupt.bit(),
            0,
            "delivered signal is blocked during its handler"
        );
    }

    #[test]
    fn apply_handler_entry_resethand_resets_action() {
        let mut state = SignalState::default();
        let mut action = custom_action(0x1000);
        action.flags = SaFlags::RESETHAND;
        state.sigaction(Signal::Interrupt, Some(action)).unwrap();
        state.apply_handler_entry(Signal::Interrupt, &action);
        assert_eq!(
            state.disposition(Signal::Interrupt),
            Disposition::DefaultTerminate,
            "RESETHAND restores the default disposition"
        );
    }

    #[test]
    fn has_fatal_deliverable_true_for_pending_term() {
        let mut state = SignalState::default();
        state.set_pending(Signal::Terminate);
        assert!(
            state.has_fatal_deliverable(),
            "pending SIGTERM under default is fatal"
        );
    }

    #[test]
    fn has_fatal_deliverable_false_when_handler_installed() {
        let mut state = SignalState::default();
        state
            .sigaction(Signal::Terminate, Some(custom_action(0x1000)))
            .unwrap();
        state.set_pending(Signal::Terminate);
        assert!(
            !state.has_fatal_deliverable(),
            "a caught signal is not fatal"
        );
    }

    #[test]
    fn has_fatal_deliverable_false_when_blocked() {
        let mut state = SignalState::default();
        state
            .sigprocmask(SigMaskHow::Block, Some(Signal::Terminate.bit()))
            .unwrap();
        state.set_pending(Signal::Terminate);
        assert!(
            !state.has_fatal_deliverable(),
            "a blocked signal is not deliverable, so not fatal"
        );
    }

    #[test]
    fn has_interrupting_deliverable_false_when_empty() {
        let state = SignalState::default();
        assert!(
            !state.has_interrupting_deliverable(),
            "no pending signal interrupts a wait"
        );
    }

    #[test]
    fn has_interrupting_deliverable_false_when_blocked() {
        let mut state = SignalState::default();
        state
            .sigprocmask(SigMaskHow::Block, Some(Signal::Terminate.bit()))
            .unwrap();
        state.set_pending(Signal::Terminate);
        assert!(
            !state.has_interrupting_deliverable(),
            "a blocked signal is not deliverable, so it does not interrupt"
        );
    }

    #[test]
    fn has_interrupting_deliverable_false_for_default_ignore() {
        let mut state = SignalState::default();
        state.set_pending(Signal::Continue);
        assert!(
            !state.has_interrupting_deliverable(),
            "an ignored signal must not cause EINTR"
        );
    }

    #[test]
    fn has_interrupting_deliverable_false_for_default_stop() {
        let mut state = SignalState::default();
        state.set_pending(Signal::TerminalStop);
        assert!(
            !state.has_interrupting_deliverable(),
            "a stop leaves the sleeper parked, it does not interrupt"
        );
    }

    #[test]
    fn has_interrupting_deliverable_true_for_handler() {
        let mut state = SignalState::default();
        state
            .sigaction(Signal::Urgent, Some(custom_action(0x1000)))
            .unwrap();
        state.set_pending(Signal::Urgent);
        assert!(
            state.has_interrupting_deliverable(),
            "a caught signal interrupts the wait"
        );
    }

    #[test]
    fn has_interrupting_deliverable_true_for_kill() {
        let mut state = SignalState::default();
        state.set_pending(Signal::Kill);
        assert!(
            state.has_interrupting_deliverable(),
            "SIGKILL always interrupts the wait"
        );
    }

    #[test]
    fn set_pending_cont_discards_stop_family() {
        let mut state = SignalState::default();
        state.set_pending(Signal::Stop);
        state.set_pending(Signal::TerminalStop);
        state.set_pending(Signal::Continue);
        assert_eq!(
            state.sigpending() & STOP_SIGNALS_MASK,
            0,
            "SIGCONT discards pending stop-family signals"
        );
        assert_ne!(
            state.sigpending() & Signal::Continue.bit(),
            0,
            "SIGCONT itself stays pending"
        );
    }

    #[test]
    fn set_pending_stop_discards_cont() {
        let mut state = SignalState::default();
        state.set_pending(Signal::Continue);
        state.set_pending(Signal::Stop);
        assert_eq!(
            state.sigpending() & Signal::Continue.bit(),
            0,
            "a stop-family signal discards pending SIGCONT"
        );
        assert_ne!(
            state.sigpending() & Signal::Stop.bit(),
            0,
            "SIGSTOP itself stays pending"
        );
    }

    #[test]
    fn deliver_cont_resumes_stopped() {
        let mut state = SignalState::default();
        state.set_stopped(true);
        let effect = state.deliver(Signal::Continue);
        assert!(effect.resume_tasks, "SIGCONT resumes a stopped process");
        assert!(!state.stopped(), "stopped flag cleared");
    }

    #[test]
    fn deliver_kill_resumes_stopped() {
        let mut state = SignalState::default();
        state.set_stopped(true);
        let effect = state.deliver(Signal::Kill);
        assert!(
            effect.resume_tasks,
            "SIGKILL wakes a stopped process so it can die"
        );
        assert!(!state.stopped(), "stopped flag cleared");
    }

    #[test]
    fn deliver_term_does_not_resume_stopped() {
        let mut state = SignalState::default();
        state.set_stopped(true);
        let effect = state.deliver(Signal::Terminate);
        assert!(
            !effect.resume_tasks,
            "SIGTERM leaves a stopped process stopped"
        );
        assert!(state.stopped(), "stopped flag unchanged");
    }

    #[test]
    fn deliver_resume_false_when_not_stopped() {
        let mut state = SignalState::default();
        let effect = state.deliver(Signal::Continue);
        assert!(
            !effect.resume_tasks,
            "SIGCONT to a running process resumes nothing"
        );
    }

    struct StatefulTestContext {
        current: TestProcess,
        processes: Vec<TestProcess>,
        states: RefCell<BTreeMap<ProcessId, SignalState>>,
        resumed: RefCell<Vec<ProcessId>>,
    }

    impl StatefulTestContext {
        fn new(current_pid: ProcessId, current_pgid: ProcessId, current_uid: u32) -> Self {
            let current = TestProcess {
                pid: current_pid,
                pgid: current_pgid,
                uid: current_uid,
            };
            Self {
                current,
                processes: vec![],
                states: RefCell::new(BTreeMap::new()),
                resumed: RefCell::new(vec![]),
            }
        }

        fn add_process(&mut self, pid: ProcessId, pgid: ProcessId, uid: u32) {
            self.processes.push(TestProcess { pid, pgid, uid });
            self.states.borrow_mut().entry(pid).or_default();
        }
    }

    impl PermissionAccess for StatefulTestContext {
        fn current_identity(&self) -> Identity {
            Identity {
                process_id: self.current.pid,
                user_id: self.current.uid,
                process_group_id: self.current.pgid,
            }
        }

        fn check_permission(&self, _target_pid: ProcessId, cap: Capability) -> Result<(), Errno> {
            assert_eq!(Capability::Signal, cap, "unexpected capability checked");
            Ok(())
        }
    }

    impl ProcessesAccess for StatefulTestContext {
        type Process = TestProcess;

        fn all_processes(&self) -> impl Iterator<Item = Self::Process> {
            self.processes.clone().into_iter()
        }
    }

    impl SignalAccess for StatefulTestContext {
        fn deliver(&self, pid: ProcessId, info: SigInfo) {
            let mut states = self.states.borrow_mut();
            let state = states.entry(pid).or_default();
            let effect = state.deliver(info.signo);
            if effect.resume_tasks {
                self.resumed.borrow_mut().push(pid);
            }
        }
    }

    #[test]
    fn flow_kill_marks_pending_and_default_terminate() {
        let current = pid!(1);
        let target = pid!(2);
        let mut cx = StatefulTestContext::new(current, current, 1000);
        cx.add_process(target, target, 1000);

        sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target),
            Signal::Terminate,
        )
        .unwrap();

        let mut states = cx.states.borrow_mut();
        let state = states.get_mut(&target).expect("target has a state");
        assert_ne!(
            state.sigpending() & Signal::Terminate.bit(),
            0,
            "SIGTERM pending after kill"
        );
        assert_eq!(
            state.take_next_deliverable(),
            Some(Signal::Terminate),
            "SIGTERM is deliverable"
        );
        assert_eq!(
            state.disposition(Signal::Terminate),
            Disposition::DefaultTerminate,
            "default disposition terminates"
        );
    }

    #[test]
    fn flow_blocked_handler_roundtrip() {
        let current = pid!(1);
        let target = pid!(2);
        let mut cx = StatefulTestContext::new(current, current, 1000);
        cx.add_process(target, target, 1000);

        {
            let mut states = cx.states.borrow_mut();
            let state = states.get_mut(&target).unwrap();
            state
                .sigaction(Signal::Interrupt, Some(custom_action(0x1000)))
                .unwrap();
            state
                .sigprocmask(SigMaskHow::Block, Some(Signal::Interrupt.bit()))
                .unwrap();
        }

        sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target),
            Signal::Interrupt,
        )
        .unwrap();

        let old_blocked;
        {
            let mut states = cx.states.borrow_mut();
            let state = states.get_mut(&target).unwrap();
            assert_ne!(
                state.sigpending() & Signal::Interrupt.bit(),
                0,
                "SIGINT pending while blocked"
            );
            assert_eq!(
                state.take_next_deliverable(),
                None,
                "blocked SIGINT is not deliverable"
            );
            state
                .sigprocmask(SigMaskHow::Unblock, Some(Signal::Interrupt.bit()))
                .unwrap();
            assert_eq!(
                state.take_next_deliverable(),
                Some(Signal::Interrupt),
                "SIGINT deliverable after unblock"
            );
            let action = match state.disposition(Signal::Interrupt) {
                Disposition::Handler(a) => a,
                other => panic!("expected handler disposition, got {other:?}"),
            };
            old_blocked = state.apply_handler_entry(Signal::Interrupt, &action);
            assert_ne!(
                state.blocked() & Signal::Interrupt.bit(),
                0,
                "SIGINT masked during its handler"
            );
        }

        sys_kill(
            &cx,
            SignalTarget::SpecificProcess(target),
            Signal::Interrupt,
        )
        .unwrap();

        {
            let mut states = cx.states.borrow_mut();
            let state = states.get_mut(&target).unwrap();
            assert_eq!(
                state.take_next_deliverable(),
                None,
                "second SIGINT stays pending while masked"
            );
            state.set_blocked_raw(old_blocked);
            assert_eq!(
                state.take_next_deliverable(),
                Some(Signal::Interrupt),
                "sigreturn restore makes the queued SIGINT deliverable"
            );
        }
    }

    #[test]
    fn flow_group_kill_reaches_members() {
        let current = pid!(1);
        let member_a = pid!(2);
        let member_b = pid!(3);
        let outsider = pid!(4);
        let group = pid!(5);

        let mut cx = StatefulTestContext::new(current, current, 1000);
        cx.add_process(member_a, group, 1000);
        cx.add_process(member_b, group, 1000);
        cx.add_process(outsider, pid!(6), 1000);

        sys_kill(&cx, SignalTarget::ProcessGroup(group), Signal::Usr1).unwrap();

        let states = cx.states.borrow();
        assert_ne!(
            states[&member_a].sigpending() & Signal::Usr1.bit(),
            0,
            "group member A received SIGUSR1"
        );
        assert_ne!(
            states[&member_b].sigpending() & Signal::Usr1.bit(),
            0,
            "group member B received SIGUSR1"
        );
        assert_eq!(
            states[&outsider].sigpending() & Signal::Usr1.bit(),
            0,
            "outsider in another group untouched"
        );
    }

    #[test]
    fn flow_stop_then_cont_resumes() {
        let current = pid!(1);
        let target = pid!(2);
        let mut cx = StatefulTestContext::new(current, current, 1000);
        cx.add_process(target, target, 1000);

        sys_kill(&cx, SignalTarget::SpecificProcess(target), Signal::Stop).unwrap();
        {
            let mut states = cx.states.borrow_mut();
            let state = states.get_mut(&target).unwrap();
            assert_eq!(
                state.take_next_deliverable(),
                Some(Signal::Stop),
                "SIGSTOP is deliverable"
            );
            state.set_stopped(true);
            assert!(state.stopped(), "target is stopped");
        }

        sys_kill(&cx, SignalTarget::SpecificProcess(target), Signal::Continue).unwrap();
        assert!(
            cx.resumed.borrow().contains(&target),
            "SIGCONT recorded a resume for the target"
        );

        let states = cx.states.borrow();
        let state = &states[&target];
        assert!(!state.stopped(), "target resumed");
        assert_eq!(
            state.sigpending() & STOP_SIGNALS_MASK,
            0,
            "SIGCONT discarded pending stop-family signals"
        );
    }

    #[test]
    fn flow_sigkill_resumes_stopped_and_is_fatal() {
        let current = pid!(1);
        let target = pid!(2);
        let mut cx = StatefulTestContext::new(current, current, 1000);
        cx.add_process(target, target, 1000);

        sys_kill(&cx, SignalTarget::SpecificProcess(target), Signal::Stop).unwrap();
        {
            let mut states = cx.states.borrow_mut();
            let state = states.get_mut(&target).unwrap();
            let _ = state.take_next_deliverable();
            state.set_stopped(true);
        }

        sys_kill(&cx, SignalTarget::SpecificProcess(target), Signal::Kill).unwrap();
        assert!(
            cx.resumed.borrow().contains(&target),
            "SIGKILL woke the stopped target"
        );

        let states = cx.states.borrow();
        let state = &states[&target];
        assert!(!state.stopped(), "target no longer stopped");
        assert!(
            state.has_fatal_deliverable(),
            "SIGKILL is a fatal deliverable"
        );
    }

    #[test]
    fn set_blocked_raw_strips_kill_stop() {
        let mut state = SignalState::default();
        state.set_blocked_raw(u64::MAX);
        assert_eq!(
            state.blocked(),
            !(Signal::Kill.bit() | Signal::Stop.bit()),
            "sigreturn restore keeps SIGKILL and SIGSTOP unblockable"
        );
    }

    #[test]
    fn sigaction_query_keeps_action() {
        let mut state = SignalState::default();
        let action = custom_action(0x1000);
        state.sigaction(Signal::Usr1, Some(action)).unwrap();
        assert_eq!(
            state.sigaction(Signal::Usr1, None),
            Ok(action),
            "query returns the installed action"
        );
        assert_eq!(
            state.disposition(Signal::Usr1),
            Disposition::Handler(action),
            "query left the installed action intact"
        );
    }

    #[test]
    fn sigprocmask_query_keeps_mask() {
        let mut state = SignalState::default();
        state
            .sigprocmask(SigMaskHow::Block, Some(Signal::Interrupt.bit()))
            .unwrap();
        assert_eq!(
            state.sigprocmask(SigMaskHow::SetMask, None),
            Ok(Signal::Interrupt.bit()),
            "query returns the current mask regardless of how"
        );
        assert_eq!(
            state.blocked(),
            Signal::Interrupt.bit(),
            "query left the mask intact"
        );
    }

    #[test]
    fn sigprocmask_setmask_strips_kill_stop() {
        let mut state = SignalState::default();
        state
            .sigprocmask(SigMaskHow::SetMask, Some(u64::MAX))
            .unwrap();
        assert_eq!(
            state.blocked(),
            !(Signal::Kill.bit() | Signal::Stop.bit()),
            "SetMask blocks everything except SIGKILL and SIGSTOP"
        );
    }

    #[test]
    fn sigaction_ignore_and_default_need_no_restorer() {
        let mut state = SignalState::default();
        let ignore = SigAction {
            handler: SigHandler::IGNORE,
            mask: 0,
            flags: SaFlags::default(),
            restorer: 0,
        };
        assert_eq!(
            state.sigaction(Signal::Usr1, Some(ignore)),
            Ok(SigAction::default()),
            "SIG_IGN needs no restorer"
        );
        let default = SigAction {
            handler: SigHandler::DEFAULT,
            mask: 0,
            flags: SaFlags::default(),
            restorer: 0,
        };
        assert_eq!(
            state.sigaction(Signal::Usr1, Some(default)),
            Ok(ignore),
            "SIG_DFL needs no restorer and returns the previous action"
        );
    }

    #[test]
    fn apply_handler_entry_merges_action_mask_and_returns_old() {
        let mut state = SignalState::default();
        state
            .sigprocmask(SigMaskHow::Block, Some(Signal::Terminate.bit()))
            .unwrap();
        let mut action = custom_action(0x1000);
        action.mask = Signal::Interrupt.bit() | Signal::Kill.bit();

        let old = state.apply_handler_entry(Signal::Usr1, &action);
        assert_eq!(
            old,
            Signal::Terminate.bit(),
            "handler entry returns the mask in force before it"
        );
        assert_eq!(
            state.blocked(),
            Signal::Terminate.bit() | Signal::Interrupt.bit() | Signal::Usr1.bit(),
            "handler entry merges the action mask with the delivered signal, minus SIGKILL"
        );
    }

    #[test]
    fn apply_handler_entry_nodefer_with_resethand() {
        let mut state = SignalState::default();
        let mut nodefer = custom_action(0x1000);
        nodefer.flags = SaFlags::NODEFER;
        state.sigaction(Signal::Usr1, Some(nodefer)).unwrap();
        state.apply_handler_entry(Signal::Usr1, &nodefer);
        assert_eq!(
            state.blocked() & Signal::Usr1.bit(),
            0,
            "NODEFER leaves SIGUSR1 unblocked during its own handler"
        );
        assert_eq!(
            state.disposition(Signal::Usr1),
            Disposition::Handler(nodefer),
            "NODEFER alone keeps the handler installed"
        );

        let mut resethand = custom_action(0x2000);
        resethand.flags = SaFlags::RESETHAND;
        state.sigaction(Signal::Usr2, Some(resethand)).unwrap();
        state.apply_handler_entry(Signal::Usr2, &resethand);
        assert_eq!(
            state.disposition(Signal::Usr2),
            Disposition::DefaultTerminate,
            "RESETHAND drops the handler back to the default action"
        );
    }

    #[test]
    fn take_next_deliverable_none_when_all_blocked() {
        let mut state = SignalState::default();
        let blocked = Signal::Interrupt.bit() | Signal::Terminate.bit();
        state.sigprocmask(SigMaskHow::Block, Some(blocked)).unwrap();
        state.set_pending(Signal::Interrupt);
        state.set_pending(Signal::Terminate);
        assert_eq!(
            state.take_next_deliverable(),
            None,
            "nothing is deliverable while every pending signal is blocked"
        );
        assert_eq!(
            state.sigpending() & blocked,
            blocked,
            "blocked signals stay pending"
        );
    }

    #[test]
    fn has_fatal_deliverable_false_for_nonfatal_pendings() {
        let mut state = SignalState::default();
        state.set_pending(Signal::WindowChanged);
        state.set_pending(Signal::TerminalStop);
        assert!(
            !state.has_fatal_deliverable(),
            "default-ignore and default-stop pendings are not fatal"
        );
        state.set_pending(Signal::Kill);
        assert!(
            state.has_fatal_deliverable(),
            "pending SIGKILL is always fatal"
        );
    }

    #[test]
    fn set_pending_discard_applies_to_blocked_signals() {
        let mut state = SignalState::default();
        state
            .sigprocmask(SigMaskHow::Block, Some(Signal::TerminalStop.bit()))
            .unwrap();
        state.set_pending(Signal::TerminalStop);
        state.set_pending(Signal::Continue);
        assert_eq!(
            state.sigpending() & Signal::TerminalStop.bit(),
            0,
            "SIGCONT discards a blocked pending SIGTSTP"
        );

        let mut state = SignalState::default();
        state
            .sigprocmask(SigMaskHow::Block, Some(Signal::Continue.bit()))
            .unwrap();
        state.set_pending(Signal::Continue);
        state.set_pending(Signal::TerminalStop);
        assert_eq!(
            state.sigpending() & Signal::Continue.bit(),
            0,
            "SIGTSTP discards a blocked pending SIGCONT"
        );
    }

    #[test]
    fn disposition_kill_fixed_and_ignore_paths() {
        let mut state = SignalState::default();
        assert_eq!(
            state.disposition(Signal::Kill),
            Disposition::DefaultTerminate,
            "SIGKILL always terminates"
        );
        let ignore = SigAction {
            handler: SigHandler::IGNORE,
            mask: 0,
            flags: SaFlags::default(),
            restorer: 0,
        };
        state.sigaction(Signal::Terminate, Some(ignore)).unwrap();
        assert_eq!(
            state.disposition(Signal::Terminate),
            Disposition::Ignore,
            "SIG_IGN resolves to ignore"
        );
        assert_eq!(
            state.disposition(Signal::WindowChanged),
            Disposition::Ignore,
            "a default-ignore signal resolves to ignore"
        );
    }

    #[test]
    fn flow_full_block_mask_never_shields_kill() {
        let current = pid!(1);
        let target = pid!(2);
        let mut cx = StatefulTestContext::new(current, current, 1000);
        cx.add_process(target, target, 1000);

        {
            let mut states = cx.states.borrow_mut();
            let state = states.get_mut(&target).unwrap();
            state
                .sigprocmask(SigMaskHow::Block, Some(u64::MAX))
                .unwrap();
        }

        sys_kill(&cx, SignalTarget::SpecificProcess(target), Signal::Kill).unwrap();

        let states = cx.states.borrow();
        let state = &states[&target];
        assert!(
            state.has_fatal_deliverable(),
            "a full block mask cannot shield the process from SIGKILL"
        );
        assert_eq!(
            state.blocked() & Signal::Kill.bit(),
            0,
            "SIGKILL is never in the blocked mask"
        );
    }
}
