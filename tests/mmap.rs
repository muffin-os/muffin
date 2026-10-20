//! End-to-end test for the anonymous `mmap` syscall.
//!
//! Pid 1 reports `mmap: ok` only if a pattern written across two anonymous
//! pages reads back. Pid 2 writes to a `PROT_READ` page, which must kill it
//! with SIGSEGV before it can report the write survived.

use test_support::{KernelTest, host_env};

#[test]
fn mmap_anon() {
    let report = KernelTest::new("mmap_anon", host_env!()).run();

    report.assert_line_contains("mmap: ok");
    report.assert_exit_code(0, 0);

    report.assert_line_contains("mmap: readonly write next");
    report.assert_no_line_contains("mmap: readonly write survived");
    report.assert_line_contains("outcome pid=2 signal=SIGSEGV");
}
