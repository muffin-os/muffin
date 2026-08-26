//! End-to-end test for guest-initiated poweroff.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/shutdown` in the
//! `/spawn` manifest. The utility issues SYS_SHUTDOWN, the kernel SIGTERM
//! sweeps, halts the APs, and writes the ACPI PM1a sleep enable, so QEMU
//! exits 0 on its own. The exit status is the whole contract: spawn marker
//! lines are racy because poweroff can beat the BSP printing them.

use test_support::{KernelTest, host_env};

#[test]
fn guest_powers_qemu_off() {
    let _transcript = KernelTest::new("shutdown", host_env!()).run_until_poweroff();
}
