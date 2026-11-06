use kernel_abi::{ECHILD, ESRCH, Errno, ProcessId, WaitOptions};
use tracing::{Level, instrument};

use crate::access::{Child, ExitOutcome, Identity, PermissionAccess, SleepAccess, WaitAccess};

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
pub fn sys_waitpid<Cx: WaitAccess + SleepAccess + PermissionAccess>(
    cx: &Cx,
    target: WaitTarget,
    options: WaitOptions,
) -> Result<Option<Reaped>, Errno> {
    let Identity {
        process_id: caller,
        process_group_id: caller_group,
        ..
    } = cx.current_identity();
    let accepts = |child: &Child| match target {
        WaitTarget::AnyChild => true,
        WaitTarget::Child(pid) => child.pid == pid,
        WaitTarget::CallerGroup => child.process_group_id == caller_group,
        WaitTarget::ProcessGroup(pgid) => child.process_group_id == pgid,
    };

    // shortcut: busy poll every 1 ms, upgrade to a parked waiter when wait gets hot
    let reaped = loop {
        let mut matching = cx.children(caller).filter(accepts).peekable();
        if matching.peek().is_none() {
            return Err(ECHILD);
        }
        if let Some(reaped) = matching.find_map(|child| {
            child.exit_outcome.map(|outcome| Reaped {
                pid: child.pid,
                outcome,
            })
        }) {
            break reaped;
        }
        if options.contains(WaitOptions::NOHANG) {
            return Ok(None);
        }
        cx.sleep_one_poll()?;
    };
    cx.remove_process(reaped.pid);
    Ok(Some(reaped))
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use kernel_abi::{ECHILD, EINTR, Signal};

    use super::*;
    use crate::access::{Capability, Identity};

    struct TestContext {
        current: ProcessId,
        processes: RefCell<Vec<(ProcessId, ProcessId, Option<ExitOutcome>)>>,
        should_interrupt_sleep: bool,
    }

    impl TestContext {
        fn new(current: ProcessId) -> Self {
            Self {
                current,
                processes: RefCell::new(Vec::new()),
                should_interrupt_sleep: false,
            }
        }

        fn add(&self, pid: ProcessId, ppid: ProcessId, outcome: Option<ExitOutcome>) {
            self.processes.borrow_mut().push((pid, ppid, outcome));
        }

        fn has(&self, pid: ProcessId) -> bool {
            self.processes.borrow().iter().any(|(p, _, _)| *p == pid)
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
                .find(|(p, _, _)| *p == pid)
                .map(|(_, ppid, _)| *ppid)
        }

        fn children(&self, parent: ProcessId) -> impl Iterator<Item = Child> {
            let children: Vec<Child> = self
                .processes
                .borrow()
                .iter()
                .filter(|(_, ppid, _)| *ppid == parent)
                .map(|(pid, _, exit_outcome)| Child {
                    pid: *pid,
                    process_group_id: *pid,
                    exit_outcome: *exit_outcome,
                })
                .collect();
            children.into_iter()
        }

        fn remove_process(&self, pid: ProcessId) {
            self.processes.borrow_mut().retain(|(p, _, _)| *p != pid);
        }
    }

    impl SleepAccess for TestContext {
        fn sleep_one_poll(&self) -> Result<(), Errno> {
            if self.should_interrupt_sleep {
                return Err(EINTR);
            }
            // The child exits during the sleep.
            for entry in self.processes.borrow_mut().iter_mut() {
                entry.2 = Some(ExitOutcome::Exited(7));
            }
            Ok(())
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
    }

    #[test]
    fn waitpid_returns_the_outcome_and_reaps() {
        let cx = TestContext::new(pid(1));
        let outcome = ExitOutcome::Signaled(Signal::Kill);
        cx.add(pid(2), pid(1), Some(outcome));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            reaped(2, outcome)
        );
        assert!(!cx.has(pid(2)), "the child leaves the process table");
    }

    #[test]
    fn waitpid_polls_until_the_child_exits() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(7))
        );
    }

    #[test]
    fn waitpid_returns_eintr_from_the_sleep() {
        let mut cx = TestContext::new(pid(1));
        cx.should_interrupt_sleep = true;
        cx.add(pid(2), pid(1), None);
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::empty()),
            Err(EINTR)
        );
        assert!(cx.has(pid(2)), "an interrupted wait keeps the child");
    }

    #[test]
    fn waitpid_any_reaps_an_exited_child_and_keeps_the_rest() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        cx.add(pid(3), pid(1), Some(ExitOutcome::Exited(3)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::AnyChild, WaitOptions::empty()),
            reaped(3, ExitOutcome::Exited(3))
        );
        assert!(cx.has(pid(2)));
        assert!(!cx.has(pid(3)));
    }

    #[test]
    fn waitpid_any_polls_until_a_child_exits() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        cx.add(pid(3), pid(1), None);
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::AnyChild, WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(7))
        );
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
    fn waitpid_group_forms_match_the_child_group() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), Some(ExitOutcome::Exited(1)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::ProcessGroup(pid(2)), WaitOptions::empty()),
            reaped(2, ExitOutcome::Exited(1))
        );

        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), Some(ExitOutcome::Exited(1)));
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::ProcessGroup(pid(9)), WaitOptions::empty()),
            Err(ECHILD)
        );
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::CallerGroup, WaitOptions::empty()),
            Err(ECHILD)
        );
    }

    #[test]
    fn waitpid_nohang_returns_none_while_the_child_runs() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::Child(pid(2)), WaitOptions::NOHANG),
            Ok(None)
        );
        assert!(
            cx.has(pid(2)),
            "the running child stays in the process table"
        );
        // The test sleep ends every child. A second `None` confirms that the call does not sleep.
        assert_eq!(
            sys_waitpid(&cx, WaitTarget::AnyChild, WaitOptions::NOHANG),
            Ok(None)
        );
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
