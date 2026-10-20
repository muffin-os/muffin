//! End-to-end test for POSIX execve.
//!
//! `/bin/execve` replaces itself with `/bin/exec-target`, which exits 42 only
//! if argv, envp, open fds and the signal disposition reset all arrived
//! intact. The ordered markers prove the swap happened, and the exit code at
//! index 0 proves the pid survived it.

use test_support::{KernelTest, host_env};

#[test]
fn execve() {
    let report = KernelTest::new("execve", host_env!()).run();

    report.assert_markers_in_order(&["execve-test: before exec", "exec-target: after exec"]);
    report.assert_exit_code(0, 42);
}
