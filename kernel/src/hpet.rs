use core::mem::MaybeUninit;
use core::ptr::NonNull;

use acpi::HpetInfo;
use bitfield::bitfield;
use conquer_once::spin::OnceCell;
use spin::RwLock;
use volatile::access::{NoAccess, ReadOnly, ReadWrite};
use volatile::{VolatileFieldAccess, VolatilePtr};
use x86_64::PhysAddr;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};

use crate::acpi::acpi_tables;
use crate::mem::address_space::AddressSpace;
use crate::mem::virt::{OwnedSegment, VirtualMemoryAllocator, VirtualMemoryHigherHalf};

static HPET: OnceCell<RwLock<Hpet>> = OnceCell::uninit();

/// # Panics
/// This function panics if the HPET is not initialized yet.
pub fn hpet() -> &'static RwLock<Hpet<'static>> {
    HPET.get().unwrap()
}

pub fn hpet_maybe() -> Option<&'static RwLock<Hpet<'static>>> {
    HPET.try_get().ok()
}

#[allow(clippy::missing_panics_doc)]
pub fn init() {
    let acpi_tables = acpi_tables();
    let guard = acpi_tables.lock();

    let hpet_info = HpetInfo::new(&guard).unwrap();
    let base_address = PhysAddr::try_new(hpet_info.base_address as u64).unwrap();

    let segment = VirtualMemoryHigherHalf.reserve(1).unwrap();
    let address_space = AddressSpace::kernel();
    address_space
        .map(
            Page::<Size4KiB>::containing_address(segment.start),
            PhysFrame::containing_address(base_address),
            PageTableFlags::PRESENT
                | PageTableFlags::NO_CACHE
                | PageTableFlags::NO_EXECUTE
                | PageTableFlags::WRITABLE,
        )
        .unwrap();

    let hpet_volatile_ptr =
        unsafe { VolatilePtr::new(NonNull::new(segment.start.as_mut_ptr()).unwrap()) };
    let hpet = Hpet {
        segment,
        inner: hpet_volatile_ptr,
    };
    hpet.enable();
    HPET.init_once(|| RwLock::new(hpet));
}

pub struct Hpet<'a> {
    #[allow(dead_code)] // upon drop, the memory segment is released
    segment: OwnedSegment<'a>,
    inner: VolatilePtr<'a, Inner>,
}

unsafe impl Send for Hpet<'_> {}
unsafe impl Sync for Hpet<'_> {}

impl Hpet<'_> {
    fn enable(&self) {
        self.inner.config().update(|mut c| {
            c.set_enable_cnf(true);
            c
        });
    }

    #[must_use]
    pub fn main_counter_value(&self) -> u64 {
        self.inner.main_counter_value().read()
    }

    #[must_use]
    pub fn period_femtoseconds(&self) -> u32 {
        self.inner.capabilities_and_id().read().counter_clk_period()
    }
}

#[repr(C)]
#[derive(Debug, VolatileFieldAccess)]
pub struct Inner {
    #[access(ReadOnly)]
    pub capabilities_and_id: HpetCapabilitiesAndId,
    #[access(NoAccess)]
    _pad1: MaybeUninit<u64>,
    #[access(ReadWrite)]
    pub config: HpetConfig,
    #[access(NoAccess)]
    _pad2: MaybeUninit<u64>,
    #[access(ReadWrite)]
    pub interrupt_status: u64,
    #[access(NoAccess)]
    _pad3: MaybeUninit<[u64; 25]>,
    #[access(ReadWrite)]
    pub main_counter_value: u64,
    #[access(NoAccess)]
    _pad4: u64,
    #[access(ReadWrite)]
    timers: [HpetTimer; 32],
}

const _: () = assert!(1280 == size_of::<Inner>());

#[repr(C)]
#[derive(Debug, VolatileFieldAccess)]
pub struct HpetTimer {
    raw: [u64; 4], // TODO: implement once we use this
}

const _: () = assert!(32 == size_of::<HpetTimer>());

bitfield! {
    #[repr(transparent)]
    #[derive(Copy, Clone)]
    pub struct HpetCapabilitiesAndId(u64);
    impl Debug;

    pub u32, counter_clk_period, _: 63, 32;
    pub u16, vendor_id, _: 31, 16;
    pub bool, legacy_replacement, _: 15;
    pub bool, count_size_cap, _: 13;
    pub u8, num_timers, _: 12, 8;
    pub u8, rev_id, _: 7, 0;
}

bitfield! {
    #[repr(transparent)]
    #[derive(Copy, Clone)]
    pub struct HpetConfig(u64);
    impl Debug;

    pub bool, legacy_replacement_cnf, set_legacy_replacement_cnf: 1;
    pub bool, enable_cnf, set_enable_cnf: 0;
}
