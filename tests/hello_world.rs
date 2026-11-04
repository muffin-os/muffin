//! End-to-end test for the std-based `hello-world` binary.
//!
//! Boots the generic `test-kernel` under QEMU with `hello-world` in
//! `/bin/tests` and asserts the greeting reaches the serial console and the
//! process exits 0 through std's runtime.

use test_support::{KernelTest, host_env};

#[test]
fn hello_world() {
    let report = KernelTest::new("hello_world", host_env!()).run();

    report.assert_line_contains("Hello, world!");
    report.assert_exit_code(0, 0);
}
