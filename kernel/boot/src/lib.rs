#![no_std]
#![no_main]
#![feature(abi_x86_interrupt, negative_impls, vec_push_within_capacity)]
extern crate alloc;

use ::limine::firmware_type::FirmwareType;
use conquer_once::spin::OnceCell;
use tracing::{Level, info, span};

use crate::driver::pci;
use crate::limine::{BOOT_TIME, EXECUTABLE_CMDLINE_REQUEST, FIRMWARE_TYPE_REQUEST};

mod acpi;
mod apic;
mod arch;
pub mod backtrace;
pub mod cmdline;
pub mod driver;
pub mod file;
pub mod hpet;
pub mod limine;
mod log;
pub mod mcore;
pub mod mem;
pub mod serial;
pub mod sse;
pub mod syscall;
pub mod time;

static BOOT_TIME_SECONDS: OnceCell<u64> = OnceCell::uninit();

/// # Panics
/// Panics if there was no boot time provided by limine.
fn init_boot_time() {
    BOOT_TIME_SECONDS.init_once(|| BOOT_TIME.get_response().unwrap().timestamp().as_secs());
}

pub fn init() {
    init_boot_time();

    cmdline::init();
    log::init();
    log_boot_environment();
    mem::init();
    acpi::init();
    apic::init();
    hpet::init();

    span!(Level::DEBUG, "kinit2").in_scope(|| {
        backtrace::init();
        mcore::init();
        file::init();
        pci::init();
    });

    info!("kernel initialized");
}

fn log_boot_environment() {
    let firmware = FIRMWARE_TYPE_REQUEST
        .get_response()
        .map_or("unknown", |resp| match resp.firmware_type() {
            FirmwareType::X86_BIOS => "x86 BIOS",
            FirmwareType::UEFI_32 => "UEFI (32-bit)",
            FirmwareType::UEFI_64 => "UEFI (64-bit)",
            FirmwareType::SBI => "SBI",
            _ => "unknown",
        });

    let cmdline = EXECUTABLE_CMDLINE_REQUEST
        .get_response()
        .and_then(|resp| resp.cmdline().to_str().ok())
        .filter(|cmdline| !cmdline.is_empty())
        .unwrap_or("(none)");

    info!("boot firmware: {firmware}, cmdline: {cmdline}");
}

#[cfg(target_pointer_width = "64")]
pub trait U64Ext {
    fn into_usize(self) -> usize;
}

#[cfg(target_pointer_width = "64")]
impl U64Ext for u64 {
    #[allow(clippy::cast_possible_truncation)]
    fn into_usize(self) -> usize {
        // Safety: we know that we are on 64-bit, so this is correct
        unsafe { usize::try_from(self).unwrap_unchecked() }
    }
}

#[cfg(target_pointer_width = "64")]
pub trait UsizeExt {
    fn into_u64(self) -> u64;
}

#[cfg(target_pointer_width = "64")]
impl UsizeExt for usize {
    fn into_u64(self) -> u64 {
        self as u64
    }
}
