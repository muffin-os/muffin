//! End-to-end test that fork copies the executable path and execve replaces it.
//!
//! `/bin/tests/exec-identity` is pid 2. It reads its executable path with
//! `SYS_EXE_PATH`, then forks. The child must report the same path as the
//! parent. The child then execs `/bin/exec-identity-target`, which must report
//! the new path and exit 42.

use test_support::{KernelTest, host_env};

#[test]
fn exec_identity() {
    let report = KernelTest::new("exec_identity", host_env!()).run();

    report.assert_markers_in_order(&[
        "exec-identity: exe=/bin/tests/exec-identity",
        "exec-identity: child exe=/bin/tests/exec-identity",
        "exec-identity: target exe=/bin/exec-identity-target",
        "exec-identity: parent waited status=42",
    ]);
    report.assert_no_line_contains("FAIL");
    report.assert_exit_code(0, 0);
}
