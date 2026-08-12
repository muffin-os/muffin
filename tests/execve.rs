//! End-to-end test for POSIX execve.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/execve` in the
//! `/spawn` manifest. The binary proves failed execs return with the old
//! image intact, then replaces itself with `/bin/exec-target`, which checks
//! argv and envp delivery, fd preservation, and the signal disposition reset.
//! Exit code 42 at index 0 proves the pid survived the image swap.

use test_support::{KernelTest, host_env};

#[test]
fn execve() {
    let report = KernelTest::new("execve", host_env!()).run();

    report.assert_markers_in_order(&["execve-test: before exec", "exec-target: after exec"]);
    report.assert_exit_code(0, 42);
}
