#![no_std]
#![no_main]
#![feature(abi_x86_interrupt, negative_impls, vec_push_within_capacity)]
extern crate alloc;

use ::log::info;
use conquer_once::spin::OnceCell;

use crate::driver::pci;
use crate::limine::BOOT_TIME;

mod acpi;
mod apic;
mod arch;
pub mod backtrace;
pub mod driver;
pub mod file;
pub mod hpet;
pub mod limine;
mod log;
pub mod mcore;
pub mod mem;
mod serial;
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

    log::init();
    mem::init();
    acpi::init();
    apic::init();
    hpet::init();
    backtrace::init();
    mcore::init();
    file::init();
    pci::init();

    info!("kernel initialized");
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
