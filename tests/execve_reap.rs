//! End-to-end test for sibling-task reaping on execve.
//!
//! The test kernel attaches an extra kernel task to the process spawned from
//! the manifest before that process's main task becomes runnable, so the
//! sibling is already attached when execve runs. The sibling parks like a task
//! blocked in a syscall. The disk reuses `/bin/execve`, so the marker order
//! proves execve blocked until that sibling observed the reap request and
//! died, and exit 42 at index 0 proves the exec then completed.

use test_support::{KernelTest, host_env};

#[test]
fn execve_reap() {
    let report = KernelTest::new("execve_reap", host_env!()).run();

    report.assert_line_contains("test-kernel: sibling attached");
    report.assert_markers_in_order(&[
        "execve-test: before exec",
        "test-kernel: sibling terminating",
        "exec-target: after exec",
    ]);
    report.assert_exit_code(0, 42);
}
