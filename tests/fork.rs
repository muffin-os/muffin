//! End-to-end test for POSIX fork.
//!
//! `/bin/fork` forks. The child increments a stack local and a static, which
//! the parent must not see, signals the parent and execs `/bin/exit-code`.
//! The parent waits for the signal and exits 42. The child's pid is not in
//! the `/spawn` tracker, so only its transcript lines are asserted.

use test_support::{KernelTest, host_env};

#[test]
fn fork() {
    let report = KernelTest::new("fork", host_env!()).run();

    report.assert_markers_in_order(&[
        "fork: child local=6 counter=2",
        "exitcode: exiting 42",
        "fork: parent local=5 counter=1",
    ]);
    report.assert_no_line_contains("terminating process");
    report.assert_exit_code(0, 42);
}
