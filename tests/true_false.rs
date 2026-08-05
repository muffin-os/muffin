//! End-to-end test for the `true` and `false` userspace utilities.
//!
//! Boots the generic `test-kernel` under QEMU with both utilities in the
//! `/spawn` manifest and asserts each reports the exit code its contract
//! promises, 0 from `true` and 1 from `false`.

use test_support::{KernelTest, host_env};

#[test]
fn utility_exit_codes() {
    let report = KernelTest::new("true_false", host_env!()).run();

    report.assert_exit_code(0, 0);
    report.assert_exit_code(1, 1);
}
