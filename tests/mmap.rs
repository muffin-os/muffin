//! End-to-end test for the anonymous `mmap` syscall.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/mmap` in the
//! `/spawn` manifest. The binary maps two anonymous pages, writes a byte
//! pattern across both, and reads it back. The test asserts on its serial
//! output and exit code.

use test_support::{KernelTest, host_env};

#[test]
fn mmap_anon() {
    let report = KernelTest::new("mmap_anon", host_env!()).run();

    report.assert_line_contains("mmap: ok");
    report.assert_exit_code(0, 0);
}
