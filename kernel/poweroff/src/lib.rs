//! Terminal ACPI power-off.
//!
//! Separate crate to make sure this doesn't allocate.

#![no_std]
#![feature(never_type)]

use x86_64::instructions::port::Port;

/// PM1 control ports resolved from the FADT.
#[derive(Debug, Copy, Clone)]
pub struct Pm1Ports {
    pub a: u16,
    pub b: Option<u16>,
}

/// SLP_EN bit of the PM1 control register (ACPI spec "PM1 Control Registers
/// Fixed Hardware Feature Control Bits"). SLP_TYP (bits 10..=12) stays 0
/// because QEMU's \_S5 sleep type is 0 and reading the real value needs an
/// AML interpreter, which the kernel does not have.
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
