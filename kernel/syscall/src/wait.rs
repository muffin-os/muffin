use kernel_abi::{ECHILD, ESRCH, Errno, ProcessId};
use tracing::{Level, instrument};

use crate::access::{PermissionAccess, WaitAccess};
use crate::ptr::UserspaceMutPtr;

#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_getppid<Cx: WaitAccess + PermissionAccess>(cx: &Cx) -> Result<usize, Errno> {
    let pid = cx.current_identity().process_id;
    let ppid = cx.parent_id(pid).ok_or(ESRCH)?;
    Ok(ppid.as_u64() as usize)
}

/// POSIX waitpid for one child pid, without options. Writes the status to
/// `status` when it is `Some` and removes the child from the process table.
///
/// # Errors
/// `ESRCH` when no process has `pid`. `ECHILD` when `pid` is not a child
/// of the caller. `EINTR` when a signal interrupts the wait.
#[instrument(level = Level::TRACE, skip(cx, status))]
pub fn sys_waitpid<Cx: WaitAccess + PermissionAccess>(
    cx: &Cx,
    pid: ProcessId,
    status: Option<UserspaceMutPtr<i32>>,
) -> Result<usize, Errno> {
    let ppid = cx.parent_id(pid).ok_or(ESRCH)?;
    if ppid != cx.current_identity().process_id {
        return Err(ECHILD);
    }

    // shortcut: busy poll every 1 ms, upgrade to a parked waiter when wait gets hot
    let outcome = loop {
        if let Some(outcome) = cx.exit_outcome(pid) {
            break outcome;
        }
        cx.sleep_one_poll()?;
    };
    if let Some(mut status) = status {
        // Safety: the dispatcher validated the pointer and made it resident.
        unsafe { status.as_mut_ptr().write(outcome.wait_status()) };
    }
    cx.remove_process(pid);
    Ok(pid.as_u64() as usize)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use kernel_abi::{ECHILD, EINTR, ESRCH, Signal};

    use super::*;
    use crate::access::{Capability, ExitOutcome, Identity};

    struct TestContext {
        current: ProcessId,
        processes: RefCell<Vec<(ProcessId, ProcessId, Option<ExitOutcome>)>>,
        interrupt_sleep: bool,
    }

    impl TestContext {
        fn new(current: ProcessId) -> Self {
            Self {
                current,
                processes: RefCell::new(Vec::new()),
                interrupt_sleep: false,
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

        fn exit_outcome(&self, pid: ProcessId) -> Option<ExitOutcome> {
            self.processes
                .borrow()
                .iter()
                .find(|(p, _, _)| *p == pid)
                .and_then(|(_, _, outcome)| *outcome)
        }

        fn remove_process(&self, pid: ProcessId) {
            self.processes.borrow_mut().retain(|(p, _, _)| *p != pid);
        }

        fn sleep_one_poll(&self) -> Result<(), Errno> {
            if self.interrupt_sleep {
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
        assert_eq!(sys_waitpid(&cx, pid(9), None), Err(ESRCH));
        assert_eq!(sys_waitpid(&cx, pid(3), None), Err(ECHILD));
    }

    #[test]
    fn waitpid_reports_the_status_and_reaps() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), Some(ExitOutcome::Signaled(Signal::Kill)));
        let mut status = 0i32;
        let ptr = UserspaceMutPtr::try_from_usize(&raw mut status as usize).unwrap();
        assert_eq!(sys_waitpid(&cx, pid(2), Some(ptr)), Ok(2));
        assert_eq!(status, 128 + Signal::Kill.number());
        assert!(!cx.has(pid(2)), "the child leaves the process table");
    }

    #[test]
    fn waitpid_polls_until_the_child_exits() {
        let cx = TestContext::new(pid(1));
        cx.add(pid(2), pid(1), None);
        let mut status = 0i32;
        let ptr = UserspaceMutPtr::try_from_usize(&raw mut status as usize).unwrap();
        assert_eq!(sys_waitpid(&cx, pid(2), Some(ptr)), Ok(2));
        assert_eq!(status, 7);
    }

    #[test]
    fn waitpid_returns_eintr_from_the_sleep() {
        let mut cx = TestContext::new(pid(1));
        cx.interrupt_sleep = true;
        cx.add(pid(2), pid(1), None);
        assert_eq!(sys_waitpid(&cx, pid(2), None), Err(EINTR));
        assert!(cx.has(pid(2)), "an interrupted wait keeps the child");
    }
}
