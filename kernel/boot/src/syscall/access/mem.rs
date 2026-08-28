use kernel_syscall::UserspacePtr;
use kernel_syscall::access::{
    AllocationStrategy, CreateMappingError, Location, Mapping, MemoryAccess,
};
use kernel_virtual_memory::Segment;
use x86_64::VirtAddr;
use x86_64::structures::paging::{PageSize, PageTableFlags, Size4KiB};

use crate::UsizeExt;
use crate::mcore::mtask::process::mem::{MemoryRegion, PrivateMemoryRegion};
use crate::mem::virt::VirtualMemoryAllocator;
use crate::syscall::access::{KernelAccess, KernelMemoryRegionHandle};

impl MemoryAccess for KernelAccess {
    type Mapping = KernelMapping;

    fn create_mapping(
        &self,
        location: Location,
        size: usize,
        _allocation_strategy: AllocationStrategy,
    ) -> Result<Self::Mapping, CreateMappingError> {
        let page_aligned_size = size.next_multiple_of(Size4KiB::SIZE as usize);
        let page_count = page_aligned_size / Size4KiB::SIZE as usize;

        let segment = if let Location::Fixed(addr) = location {
            self.process
                .vmm()
                .mark_as_reserved(Segment::new(
                    VirtAddr::from_ptr(addr.as_ptr()),
                    page_aligned_size.into_u64(),
                ))
                .map_err(|_| CreateMappingError::LocationAlreadyMapped)?
        } else {
            self.process
                .vmm()
                .reserve(page_count)
                .ok_or(CreateMappingError::OutOfMemory)?
        };

        // The region spans the page-aligned size so the fault handler can
        // serve the last partial page.
        let region = MemoryRegion::Private(PrivateMemoryRegion::new(
            segment,
            page_aligned_size,
            PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE
                | PageTableFlags::NO_EXECUTE,
        ));

        Ok(KernelMapping {
            addr: region.addr(),
            size: page_aligned_size,
            region,
        })
    }
}

pub struct KernelMapping {
    addr: VirtAddr,
    size: usize,
    region: MemoryRegion,
}

impl KernelMapping {
    /// Convert this mapping into a MemoryRegion handle that can be tracked by the process.
    pub fn into_region_handle(self) -> KernelMemoryRegionHandle {
        let addr = self
            .addr
            .as_ptr::<u8>()
            .try_into()
            .expect("kernel mapping should be located in user space");

        KernelMemoryRegionHandle {
            addr,
            size: self.size,
            inner: self.region,
        }
    }
}

impl Mapping for KernelMapping {
    fn addr(&self) -> UserspacePtr<u8> {
        self.addr
            .as_ptr::<u8>()
            .try_into()
            .expect("kernel mapping should be located in user space")
    }

    fn size(&self) -> usize {
        self.size
    }
}
