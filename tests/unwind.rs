//! End-to-end test for userspace panic unwinding and backtrace symbolization.
//!
//! A resolved function name in the transcript covers the whole chain. The process
//! found its own executable, read it back, and matched a return address against its
//! tables. The source position beside it comes from DWARF, which an optimized build
//! strips, so the assertion must not reach for a file or a line.

use test_support::{KernelTest, host_env};

#[test]
fn unwind() {
    let report = KernelTest::new("unwind", host_env!()).run();

    report.assert_markers_in_order(&["unwind: caught", "unwind: caught twice", "panicked at"]);
    report.assert_line_contains("stack backtrace:");
    report.assert_line_contains("_start");
    report.assert_exit_code(0, 101);
}
