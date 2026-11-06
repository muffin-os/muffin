use kernel_abi::{ESRCH, Errno, ProcessId, WaitOptions};
use tracing::{Level, instrument};

use crate::access::{ExitOutcome, Identity, PermissionAccess, WaitAccess};

#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_getppid<Cx: WaitAccess + PermissionAccess>(cx: &Cx) -> Result<usize, Errno> {
    let pid = cx.current_identity().process_id;
    let ppid = cx.parent_id(pid).ok_or(ESRCH)?;
    Ok(ppid.as_u64() as usize)
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum WaitTarget {
    AnyChild,
    Child(ProcessId),
    CallerGroup,
    ProcessGroup(ProcessId),
}

/// This type describes a child that `sys_waitpid` removes from the process table.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Reaped {
    pub pid: ProcessId,
    pub outcome: ExitOutcome,
}

/// This function implements POSIX waitpid.
/// It removes the reaped child from the process table and returns the child.
/// With `WaitOptions::NOHANG`, it returns `None` when a child matches `target` but no matching child has an exit outcome.
///
/// # Errors
/// This function returns `ECHILD` when no child matches `target`. It returns
/// `EINTR` when a signal interrupts the wait.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_waitpid<Cx: WaitAccess + PermissionAccess>(
    cx: &Cx,
    target: WaitTarget,
    options: WaitOptions,
) -> Result<Option<Reaped>, Errno> {
    let Identity {
        process_id: caller,
        process_group_id: caller_group,
        ..
    } = cx.current_identity();

    let mut result = cx.try_reap_child(caller, caller_group, target);
    loop {
        if !matches!(&result, Ok(None)) || options.contains(WaitOptions::NOHANG) {
            return result;
        }
        let waited = cx.wait_for_child_change(|| {
            result = cx.try_reap_child(caller, caller_group, target);
            !matches!(&result, Ok(None))
        });
        // A reap in the predicate consumes the exit record, so it wins over an interruption.
        if !matches!(&result, Ok(None)) {
            return result;
        }
        waited?;
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};

    use kernel_abi::{ECHILD, EINTR, Signal};

    use super::*;
    use crate::access::{Capability, Identity};

    #[derive(Clone, Copy)]
    struct Entry {
        pid: ProcessId,
        ppid: ProcessId,
        pgid: ProcessId,
        outcome: Option<ExitOutcome>,
    }

    /// One event that a wait observes.
    #[derive(Clone, Copy)]
    enum Event {
        /// A child of the caller exits.
        Exit(ProcessId, ExitOutcome),
        /// A signal interrupts the wait.
        Signal,
        /// A child exits and a signal arrives before the waiter runs.
        ExitAndSignal(ProcessId, ExitOutcome),
    }

    struct TestContext {
        current: ProcessId,
        processes: RefCell<Vec<Entry>>,
        events: RefCell<Vec<Event>>,
        waits: Cell<usize>,
    }

    impl TestContext {
        fn new(current: ProcessId) -> Self {
            Self {
                current,
                processes: RefCell::new(Vec::new()),
                events: RefCell::new(Vec::new()),
                waits: Cell::new(0),
            }
        }

        fn add(&self, pid: ProcessId, ppid: ProcessId, outcome: Option<ExitOutcome>) {
            self.add_in_group(pid, ppid, pid, outcome);
        }

        fn add_in_group(
            &self,
            pid: ProcessId,
            ppid: ProcessId,
            pgid: ProcessId,
            outcome: Option<ExitOutcome>,
        ) {
            self.processes.borrow_mut().push(Entry {
                pid,
                ppid,
                pgid,
                outcome,
            });
        }

        fn on_wait(&self, event: Event) {
            self.events.borrow_mut().push(event);
        }

        fn has(&self, pid: ProcessId) -> bool {
            self.processes.borrow().iter().any(|e| e.pid == pid)
        }

        fn exit(&self, pid: ProcessId, outcome: ExitOutcome) {
            for entry in self.processes.borrow_mut().iter_mut() {
                if entry.pid == pid {
                    entry.outcome = Some(outcome);
                }
            }
        }
    }

    impl PermissionAccess for TestContext {
        fn current_identity(&self) -> Identity {
            Identity {
                process_id: self.current,
                user_id: 0,
                process_group_id: self.current,
            }
        }

        fn check_permission(&self, _: ProcessId, _: Capability) -> Result<(), Errno> {
            Ok(())
        }
    }

    impl WaitAccess for TestContext {
        fn parent_id(&self, pid: ProcessId) -> Option<ProcessId> {
            self.processes
                .borrow()
                .iter()
                .find(|e| e.pid == pid)
                .map(|e| e.ppid)
        }

        fn try_reap_child(
            &self,
            parent: ProcessId,
            caller_group: ProcessId,
            target: WaitTarget,
        ) -> Result<Option<Reaped>, Errno> {
            let mut processes = self.processes.borrow_mut();
            let mut matching: Vec<Entry> = processes
                .iter()
                .filter(|e| e.ppid == parent)
                .filter(|e| match target {
                    WaitTarget::AnyChild => true,
                    WaitTarget::Child(pid) => e.pid == pid,
                    WaitTarget::CallerGroup => e.pgid == caller_group,
                    WaitTarget::ProcessGroup(pgid) => e.pgid == pgid,
                })
                .copied()
                .collect();
            if matching.is_empty() {
                return Err(ECHILD);
            }
            matching.sort_by_key(|e| e.pid);
            let Some(reaped) = matching.iter().find_map(|e| {
                e.outcome.map(|outcome| Reaped {
                    pid: e.pid,
                    outcome,
                })
            }) else {
                return Ok(None);
            };
            processes.retain(|e| e.pid != reaped.pid);
            Ok(Some(reaped))
        }

        fn wait_for_child_change(
            &self,
            mut should_wake: impl FnMut() -> bool,
        ) -> Result<(), Errno> {
            self.waits.set(self.waits.get() + 1);
            if should_wake() {
                return Ok(());
            }
            let event = self.events.borrow_mut().remove(0);
            let is_interrupted = match event {
                Event::Exit(pid, outcome) => {
                    self.exit(pid, outcome);
                    false
                }
                Event::Signal => true,
                Event::ExitAndSignal(pid, outcome) => {
                    self.exit(pid, outcome);
                    true
                }
            };
            // The child predicate runs before the signal check.
            if should_wake() {
                return Ok(());
            }
            if is_interrupted { Err(EINTR) } else { Ok(()) }
        }
    }

    fn pid(n: u64) -> ProcessId {
        ProcessId::from(n)
    }

    fn reaped(n: u64, outcome: ExitOutcome) -> Result<Option<Reaped>, Errno> {
        Ok(Some(Reaped {
            pid: pid(n),
            outcome,
        }))
    }

    #[test]
    fn getppid_returns_the_parent() {
        let cx = TestContext::new(pid(2));
        cx.add(pid(2), pid(1), None);
        assert_eq!(sys_getppid(&cx), Ok(1));
    }

    #[test]
    fn waitpid_rejects_unknown_and_foreign_pids() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(3), pid(2), None);
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(9)), WaitOptions::empty()),
            Err(ECHILD)
        );
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(3)), WaitOptions::empty()),
            Err(ECHILD)
        );
        assert_eq!(cx.waits.get(), 0);
    }

    #[test]
    fn waitpid_returns_the_outcome_and_reaps_without_a_wait() {
        let cx = TestContext::new(pid(1));
        let outcome = ExitOutcome::Signaled(Signal::Kill);
        cx.add(pid(2), pid(1), Some(outcome));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            reaped(2, outcome)
        );
        assert!(!cx.has(pid(2)), "the child leaves the process table");
        assert_eq!(cx.waits.get(), 0);
    }

    #[test]
    fn waitpid_waits_for_the_child_change() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        cx.on_wait(Event::Exit(pid(2), ExitOutcome::Exited(7)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(7))
        );
        assert!(!cx.has(pid(2)));
        assert_eq!(cx.waits.get(), 1);
    }

    #[test]
    fn waitpid_ignores_unrelated_child_changes() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        cx.add(pid(3), pid(1), None);
        cx.on_wait(Event::Exit(pid(3), ExitOutcome::Exited(3)));
        cx.on_wait(Event::Exit(pid(2), ExitOutcome::Exited(2)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(2))
        );
        assert!(cx.has(pid(3)), "the other exit record stays");
        assert_eq!(cx.waits.get(), 2);
    }

    #[test]
    fn waitpid_returns_eintr_from_the_wait() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        cx.on_wait(Event::Signal);
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            Err(EINTR)
        );
        assert!(cx.has(pid(2)), "an interrupted wait keeps the child");
    }

    #[test]
    fn waitpid_completed_reap_wins_over_interruption() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        cx.on_wait(Event::ExitAndSignal(pid(2), ExitOutcome::Exited(4)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::AnyChild, WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(4))
        );
        assert!(!cx.has(pid(2)));
    }

    #[test]
    fn waitpid_any_reaps_the_lowest_exited_child_and_keeps_the_rest() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(4), pid(1), Some(ExitOutcome::Exited(4)));
        cx.add(pid(2), pid(1), None);
        cx.add(pid(3), pid(1), Some(ExitOutcome::Exited(3)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::AnyChild, WaitOptions::empty()),
            reaped(3, ExitOutcome::Exited(3))
        );
        assert!(cx.has(pid(2)));
        assert!(!cx.has(pid(3)));
        assert!(cx.has(pid(4)));
    }

    #[test]
    fn waitpid_any_without_children_is_echild() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(3), pid(2), None);
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::AnyChild, WaitOptions::empty()),
            Err(ECHILD)
        );
    }

    #[test]
    fn waitpid_reports_echild_after_a_competing_reap() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), Some(ExitOutcome::Exited(0)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(0))
        );
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            Err(ECHILD)
        );
    }

    #[test]
    fn waitpid_group_forms_match_the_child_group() {
        let cx = TestContext::new(pid(1));
        cx.add_in_group(pid(2), pid(1), pid(5), Some(ExitOutcome::Exited(1)));
        cx.add_in_group(pid(3), pid(1), pid(1), Some(ExitOutcome::Exited(2)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::ProcessGroup(pid(9)), WaitOptions::empty()),
            Err(ECHILD)
        );
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::ProcessGroup(pid(5)), WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(1))
        );
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::CallerGroup, WaitOptions::empty()),
            reaped(3, ExitOutcome::Exited(2))
        );
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::CallerGroup, WaitOptions::empty()),
            Err(ECHILD)
        );
    }

    #[test]
    fn waitpid_group_form_waits_for_a_group_member() {
        let cx = TestContext::new(pid(1));
        cx.add_in_group(pid(2), pid(1), pid(5), None);
        cx.on_wait(Event::Exit(pid(2), ExitOutcome::Exited(6)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::ProcessGroup(pid(5)), WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(6))
        );
    }

    #[test]
    fn waitpid_nohang_returns_none_without_a_wait() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        for target in [
            WaitTarget::AnyChild,
            WaitTarget::Child(pid(2)),
            WaitTarget::CallerGroup,
            WaitTarget::ProcessGroup(pid(2)),
        ] {
            let cx = TestContext::new(pid(1));
            cx.add_in_group(pid(2), pid(1), pid(1), None);
            let target = match target {
                WaitTarget::ProcessGroup(_) => WaitTarget::ProcessGroup(pid(1)),
                other => other,
            };
            assert_eq!(sys_waitpid(&cx, target, WaitOptions::NOHANG), Ok(None));
            assert_eq!(cx.waits.get(), 0);
        }
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::NOHANG),
            Ok(None)
        );
        assert!(
            cx.has(pid(2)),
            "the running child stays in the process table"
        );
        assert_eq!(cx.waits.get(), 0);
    }

    #[test]
    fn waitpid_nohang_reaps_an_exited_child() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        cx.add(pid(3), pid(1), Some(ExitOutcome::Exited(5)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::AnyChild, WaitOptions::NOHANG),
            reaped(3, ExitOutcome::Exited(5))
        );
        assert!(cx.has(pid(2)));
        assert!(!cx.has(pid(3)));
    }

    #[test]
    fn waitpid_nohang_without_children_is_echild() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(3), pid(2), None);
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::AnyChild, WaitOptions::NOHANG),
            Err(ECHILD)
        );
    }
}
