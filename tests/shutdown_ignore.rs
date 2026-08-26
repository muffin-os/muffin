//! End-to-end test that SIGTERM is not a shutdown veto.
//!
//! pid 1 sets the SIGTERM disposition to ignore and never exits. pid 2 issues
//! SYS_SHUTDOWN. The kernel's SIGTERM sweep therefore has a survivor, so the
//! bounded grace expires, the APs are halted, and ACPI power-off happens
//! anyway. QEMU exiting 0 before the harness deadline is the whole assertion:
//! an ignoring process can delay shutdown by at most the grace, never block
//! it. Wall-clock timing is deliberately not asserted because TCG makes it
//! flaky.

use test_support::{KernelTest, host_env};

#[test]
fn sigterm_ignorer_cannot_block_poweroff() {
    let _transcript = KernelTest::new("shutdown_ignore", host_env!()).run_until_poweroff();
}
