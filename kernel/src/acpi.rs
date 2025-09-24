use core::ptr::NonNull;

use acpi::{AcpiHandler, AcpiTables, PhysicalMapping};
use conquer_once::spin::OnceCell;
use kernel_virtual_memory::Segment;
use spin::Mutex;
use x86_64::structures::paging::{Page, PageSize, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use crate::U64Ext;
use crate::limine::RSDP_REQUEST;
use crate::mem::address_space::AddressSpace;
use crate::mem::virt::{VirtualMemoryAllocator, VirtualMemoryHigherHalf};

static ACPI_TABLES: OnceCell<Mutex<AcpiTables<AcpiHandlerImpl>>> = OnceCell::uninit();

pub fn acpi_tables() -> &'static Mutex<AcpiTables<AcpiHandlerImpl>> {
    ACPI_TABLES
        .get()
        .expect("ACPI tables should be initialized")
}

pub fn init() {
    ACPI_TABLES.init_once(|| {
        let rsdp = PhysAddr::new(RSDP_REQUEST.get_response().unwrap().address() as u64);
        let tables = unsafe { AcpiTables::from_rsdp(AcpiHandlerImpl, rsdp.as_u64().into_usize()) }
            .expect("should be able to get ACPI tables from rsdp");

        Mutex::new(tables)
    });
}

#[derive(Debug, Copy, Clone)]
pub struct AcpiHandlerImpl;

impl AcpiHandler for AcpiHandlerImpl {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        assert!(size <= Size4KiB::SIZE.into_usize());
        assert!(size_of::<T>() <= Size4KiB::SIZE.into_usize());

        let phys_addr = PhysAddr::new(physical_address as u64);

        let segment = VirtualMemoryHigherHalf.reserve(1).unwrap().leak();

        let address_space = AddressSpace::kernel();
        address_space
            .map(
                Page::<Size4KiB>::containing_address(segment.start),
                PhysFrame::containing_address(phys_addr),
                PageTableFlags::PRESENT | PageTableFlags::NO_EXECUTE | PageTableFlags::WRITABLE,
            )
            .expect("should be able to map the ACPI region");

        unsafe {
            PhysicalMapping::new(
                physical_address,
                NonNull::new(segment.start.as_mut_ptr()).unwrap(),
                size,
                segment.len.into_usize(),
                Self,
            )
        }
    }

    fn unmap_physical_region<T>(region: &PhysicalMapping<Self, T>) {
        let vaddr = VirtAddr::from_ptr(region.virtual_start().as_ptr());

        let address_space = AddressSpace::kernel();
        // don't deallocate physical, because we don't manage it - it's ACPI memory
        address_space
            .unmap(Page::<Size4KiB>::containing_address(vaddr))
            .expect("address should have been mapped");

        let segment = Segment::new(vaddr, region.mapped_length() as u64);
        unsafe {
            assert!(VirtualMemoryHigherHalf.release(segment));
        }
    }
}
