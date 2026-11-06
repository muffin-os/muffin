//! End-to-end test for kernel backtrace symbolization from the `.ksym` module.

use test_support::{KernelTest, host_env};

#[test]
fn kernel_backtrace_names_the_probe_function() {
    let report = KernelTest::new("backtrace", host_env!()).run();

    report.assert_line_contains("test-kernel: frame");
    report.assert_line_contains("probe_backtrace+0x");
    report.assert_no_line_contains("no kernel symbol module");
    report.assert_no_line_contains("invalid kernel symbol module");
    report.assert_exit_code(0, 42);
}
