//! End-to-end test for multi-process fan-out and exit code reporting.
//!
//! Boots the generic `test-kernel` under QEMU with two different binaries in
//! the `/spawn` manifest, proving several processes can be booted in a single
//! QEMU instance. The test asserts each process reports its own exit code, 42
//! from `/bin/exit-code` and 0 from `/bin/file-read`.

use test_support::{KernelTest, host_env};

#[test]
fn process_exit_codes() {
    let report = KernelTest::new("exit_codes", host_env!()).run();

    report.assert_exit_code(0, 42);
    report.assert_exit_code(1, 0);
}
