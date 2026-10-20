//! Terminal ACPI power-off.

#![no_std]
#![feature(never_type)]

use x86_64::instructions::port::Port;

/// PM1 control ports resolved from the FADT.
#[derive(Debug, Copy, Clone)]
pub struct Pm1Ports {
    pub a: u16,
    pub b: Option<u16>,
}

// SLP_EN bit of the PM1 control register (ACPI spec "PM1 Control Registers
// Fixed Hardware Feature Control Bits").
// shortcut: SLP_TYP (bits 10..=12) is left 0, which is QEMU's \_S5 value,
// until the kernel has an AML interpreter to read the real one from the DSDT.
const SLP_EN: u16 = 1 << 13;

/// The hardware ignored the SLP_EN write, so the machine is still running.
#[derive(Debug)]
pub struct PowerOffIgnored;

/// Enters ACPI S5 by writing SLP_EN to the PM1 control ports.
///
/// A successful write cuts power. Returning `Err` means the hardware ignored
/// the write.
pub fn power_off(ports: Pm1Ports) -> Result<!, PowerOffIgnored> {
    unsafe {
        Port::<u16>::new(ports.a).write(SLP_EN);
        if let Some(b) = ports.b {
            Port::<u16>::new(b).write(SLP_EN);
        }
    }
    Err(PowerOffIgnored)
}
