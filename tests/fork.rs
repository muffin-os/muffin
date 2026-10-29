//! End-to-end test for POSIX fork, getppid and waitpid.
//!
//! `/bin/fork` forks. The child prints its pid and parent pid, increments a
//! stack local and a static, which the parent must not see, and execs
//! `/bin/exit-code`, which exits 42. The parent waits for the child with
//! `waitpid`, prints the status 42 and exits 42. The child's pid is not in
//! the `/spawn` tracker, so only its transcript lines are asserted.

use test_support::{KernelTest, host_env};

#[test]
fn fork() {
    let report = KernelTest::new("fork", host_env!()).run();

    report.assert_line_contains("fork: child pid=2 ppid=1");
    report.assert_markers_in_order(&[
        "fork: child local=6 counter=2",
        "exitcode: exiting 42",
        "fork: parent waited pid=2 status=42",
        "fork: parent local=5 counter=1",
    ]);
    report.assert_no_line_contains("terminating process");
    report.assert_exit_code(0, 42);
}
