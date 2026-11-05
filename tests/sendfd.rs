//! Runs `/bin/tests/sendfd` in the guest.

use test_support::{KernelTest, host_env};

#[test]
fn sendfd() {
    let report = KernelTest::new("sendfd", host_env!()).run();

    report.assert_no_line_contains("FAIL");
    report.assert_line_contains("sendfd: ok");
    report.assert_exit_code(0, 0);
}
