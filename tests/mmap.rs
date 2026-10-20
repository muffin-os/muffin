//! End-to-end test for the anonymous `mmap` syscall.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/mmap` twice in the
//! `/spawn` manifest. Pid 1 maps two anonymous pages, writes a byte pattern
//! across both, and reads it back. Pid 2 maps a `PROT_READ` page, reads it,
//! then writes to it, which must kill it with SIGSEGV. The test asserts on
//! their serial output and outcomes.

use test_support::{KernelTest, host_env};

#[test]
fn mmap_anon() {
    let report = KernelTest::new("mmap_anon", host_env!()).run();

    report.assert_line_contains("mmap: ok");
    report.assert_exit_code(0, 0);

    report.assert_line_contains("mmap: readonly write next");
    report.assert_no_line_contains("mmap: readonly write survived");
    report.assert_line_contains("terminating process on signal SIGSEGV (pid 2)");
}
