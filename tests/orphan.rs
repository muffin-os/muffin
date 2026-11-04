//! End-to-end test for POSIX `waitpid(-1)` and orphan re-parenting.
//!
//! `/bin/orphan` is pid 2. It forks pid 3, which forks pid 4 and exits
//! without a wait. Pid 2 reaps pid 3 with `waitpid(-1)`, then gets `ECHILD`
//! because pid 4 now belongs to pid 1. Pid 4 reports its new parent and
//! signals pid 2, which exits 0 after that.

use test_support::{KernelTest, host_env};

#[test]
fn orphan() {
    let report = KernelTest::new("orphan", host_env!()).run();

    report.assert_line_contains("orphan: reaped pid=3 status=0");
    report.assert_line_contains("orphan: second wait ECHILD");
    report.assert_line_contains("orphan: grandchild ppid=1");
    report.assert_line_contains("orphan: grandchild done");
    report.assert_exit_code(0, 0);
}
