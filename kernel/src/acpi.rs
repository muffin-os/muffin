use core::ptr::NonNull;

use acpi::{AcpiTables, Handler, PhysicalMapping, aml::AmlError};
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

impl Handler for AcpiHandlerImpl {
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

        PhysicalMapping {
            physical_start: physical_address,
            virtual_start: NonNull::new(segment.start.as_mut_ptr()).unwrap(),
            region_length: size,
            mapped_length: segment.len.into_usize(),
            handler: Self,
        }
    }

    fn unmap_physical_region<T>(region: &PhysicalMapping<Self, T>) {
        let vaddr = VirtAddr::from_ptr(region.virtual_start.as_ptr());

        let address_space = AddressSpace::kernel();
        // don't deallocate physical, because we don't manage it - it's ACPI memory
        address_space
            .unmap(Page::<Size4KiB>::containing_address(vaddr))
            .expect("address should have been mapped");

        let segment = Segment::new(vaddr, region.mapped_length as u64);
        unsafe {
            assert!(VirtualMemoryHigherHalf.release(segment));
        }
    }

    // Memory-mapped I/O operations
    fn read_u8(&self, address: usize) -> u8 {
        unsafe { core::ptr::read_volatile(address as *const u8) }
    }

    fn read_u16(&self, address: usize) -> u16 {
        unsafe { core::ptr::read_volatile(address as *const u16) }
    }

    fn read_u32(&self, address: usize) -> u32 {
        unsafe { core::ptr::read_volatile(address as *const u32) }
    }

    fn read_u64(&self, address: usize) -> u64 {
        unsafe { core::ptr::read_volatile(address as *const u64) }
    }

    fn write_u8(&self, address: usize, value: u8) {
        unsafe { core::ptr::write_volatile(address as *mut u8, value) }
    }

    fn write_u16(&self, address: usize, value: u16) {
        unsafe { core::ptr::write_volatile(address as *mut u16, value) }
    }

    fn write_u32(&self, address: usize, value: u32) {
        unsafe { core::ptr::write_volatile(address as *mut u32, value) }
    }

    fn write_u64(&self, address: usize, value: u64) {
        unsafe { core::ptr::write_volatile(address as *mut u64, value) }
    }

    // Port I/O operations
    fn read_io_u8(&self, port: u16) -> u8 {
        let mut port_io = x86_64::instructions::port::Port::<u8>::new(port);
        unsafe { port_io.read() }
    }

    fn read_io_u16(&self, port: u16) -> u16 {
        let mut port_io = x86_64::instructions::port::Port::<u16>::new(port);
        unsafe { port_io.read() }
    }

    fn read_io_u32(&self, port: u16) -> u32 {
        let mut port_io = x86_64::instructions::port::Port::<u32>::new(port);
        unsafe { port_io.read() }
    }

    fn write_io_u8(&self, port: u16, value: u8) {
        let mut port_io = x86_64::instructions::port::Port::<u8>::new(port);
        unsafe { port_io.write(value) }
    }

    fn write_io_u16(&self, port: u16, value: u16) {
        let mut port_io = x86_64::instructions::port::Port::<u16>::new(port);
        unsafe { port_io.write(value) }
    }

    fn write_io_u32(&self, port: u16, value: u32) {
        let mut port_io = x86_64::instructions::port::Port::<u32>::new(port);
        unsafe { port_io.write(value) }
    }

    // PCI configuration space operations
    fn read_pci_u8(&self, _address: acpi::PciAddress, _offset: u16) -> u8 {
        unimplemented!("PCI config space reads not implemented")
    }

    fn read_pci_u16(&self, _address: acpi::PciAddress, _offset: u16) -> u16 {
        unimplemented!("PCI config space reads not implemented")
    }

    fn read_pci_u32(&self, _address: acpi::PciAddress, _offset: u16) -> u32 {
        unimplemented!("PCI config space reads not implemented")
    }

    fn write_pci_u8(&self, _address: acpi::PciAddress, _offset: u16, _value: u8) {
        unimplemented!("PCI config space writes not implemented")
    }

    fn write_pci_u16(&self, _address: acpi::PciAddress, _offset: u16, _value: u16) {
        unimplemented!("PCI config space writes not implemented")
    }

    fn write_pci_u32(&self, _address: acpi::PciAddress, _offset: u16, _value: u32) {
        unimplemented!("PCI config space writes not implemented")
    }

    // Timing operations
    fn nanos_since_boot(&self) -> u64 {
        // TODO: implement proper timing using HPET or TSC
        0
    }

    fn stall(&self, microseconds: u64) {
        // Simple busy-wait stall
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        let cycles = microseconds * 3000; // Rough estimate: 3 GHz CPU
        while unsafe { core::arch::x86_64::_rdtsc() } - start < cycles {
            core::hint::spin_loop();
        }
    }

    fn sleep(&self, milliseconds: u64) {
        // For now, just stall (busy-wait)
        // TODO: implement proper sleep using timer interrupts
        self.stall(milliseconds * 1000);
    }

    // AML mutex operations
    fn create_mutex(&self) -> acpi::Handle {
        unimplemented!("AML mutex operations not implemented")
    }

    fn acquire(&self, _mutex: acpi::Handle, _timeout: u16) -> Result<(), AmlError> {
        unimplemented!("AML mutex operations not implemented")
    }

    fn release(&self, _mutex: acpi::Handle) {
        unimplemented!("AML mutex operations not implemented")
    }
}
