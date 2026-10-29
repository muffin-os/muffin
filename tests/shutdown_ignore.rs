//! End-to-end test that SIGTERM is not a shutdown veto.
//!
//! pid 2 sets the SIGTERM disposition to ignore and never exits. pid 3 issues
//! SYS_SHUTDOWN. The kernel's SIGTERM sweep therefore has a survivor, so the
//! bounded grace expires, the APs are halted, and ACPI power-off happens
//! anyway. QEMU exiting 0 before the harness deadline proves an ignoring
//! process can delay shutdown by at most the grace, never block it. The
//! marker proves the ignore was installed. The same SIGTERM kills test-init,
//! so no outcome line can prove the ignore was honoured. Wall-clock timing is
//! deliberately not asserted because TCG makes it flaky.

use test_support::{KernelTest, host_env};

#[test]
fn sigterm_ignorer_cannot_block_poweroff() {
    let transcript = KernelTest::new("shutdown_ignore", host_env!()).run_until_poweroff();

    assert!(
        transcript
            .iter()
            .any(|line| line.contains("term-ignorer: ignoring sigterm")),
        "SIGTERM was never set to ignore, transcript: {transcript:#?}"
    );
}
