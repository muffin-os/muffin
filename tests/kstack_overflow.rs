//! End-to-end test for kernel stack overflow containment.
//!
//! The test kernel spawns a kernel task that recurses until it walks into its
//! kernel stack guard page. The fault handler has to report the overflow and
//! terminate only that task. Everything else has to keep running, which the
//! spawned userspace process reaching its own exit proves.
//!
//! The harness fails the run on any line containing `panicked` and on QEMU
//! exiting before every spawned process reported an outcome, so a kernel that
//! dies on the overflow fails here without a dedicated assertion.

use test_support::{KernelTest, host_env};

#[test]
fn kernel_stack_overflow_terminates_only_the_faulting_task() {
    let report = KernelTest::new("kstack_overflow", host_env!()).run();

    report.assert_line_contains("KERNEL STACK OVERFLOW DETECTED");
    report.assert_exit_code(0, 42);
}
