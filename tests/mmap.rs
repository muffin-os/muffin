//! End-to-end test for the anonymous `mmap` syscall.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/mmap` in the
//! `/spawn` manifest. The binary maps two anonymous pages, writes a byte
//! pattern across both, and reads it back. The test asserts on its serial
//! output and exit code.
//!
//! Runs are skipped on hosts without QEMU so local checkouts stay green.
//! CI installs `qemu-system-x86`.

use test_support::{KernelTest, host_env};

#[test]
fn mmap_anon() {
    let report = KernelTest::new("mmap_anon", host_env!())
        .program("bin/mmap", env!("CARGO_BIN_FILE_MMAP_TEST_mmap-test"))
        .spawn("/bin/mmap")
        .run();

    report.assert_line_contains("mmap: ok");
    report.assert_exit_code(0, 0);
}
