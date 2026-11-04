//! End-to-end test for sibling-task reaping on execve.
//!
//! The test kernel attaches an extra kernel task to the init process before
//! that process's main task becomes runnable, so the sibling is already
//! attached when execve runs. The sibling parks like a task blocked in a
//! syscall. The disk puts `/bin/execve` at `/bin/init`, so the marker order
//! proves execve blocked until that sibling observed the reap request and
//! died, and the `after exec` marker proves the exec then completed. The
//! replacement image is pid 1, so it shuts the machine down instead of
//! exiting.

use test_support::{KernelTest, host_env};

#[test]
fn execve_reap() {
    let transcript = KernelTest::new("execve_reap", host_env!()).run_until_poweroff();

    assert!(
        transcript
            .iter()
            .any(|line| line.contains("test-kernel: sibling attached")),
        "sibling was not attached"
    );
    let position = |marker: &str| {
        transcript
            .iter()
            .position(|line| line.contains(marker))
            .unwrap_or_else(|| panic!("marker {marker:?} not found"))
    };
    let before = position("execve-test: before exec");
    let terminating = position("test-kernel: sibling terminating");
    let after = position("exec-target: after exec");
    assert!(
        before < terminating && terminating < after,
        "markers out of order: before={before} terminating={terminating} after={after}"
    );
}
