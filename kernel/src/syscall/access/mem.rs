use kernel_syscall::UserspacePtr;
use kernel_syscall::access::{
    AllocationStrategy, CreateMappingError, Location, Mapping, MemoryAccess,
};
use kernel_virtual_memory::Segment;
use x86_64::VirtAddr;
use x86_64::structures::paging::{PageSize, PageTableFlags, Size4KiB};

use crate::UsizeExt;
use crate::mcore::mtask::process::mem::{MappedMemoryRegion, MemoryRegion};
use crate::mem::phys::PhysicalMemory;
use crate::mem::virt::{OwnedSegment, VirtualMemoryAllocator};
use crate::syscall::access::{KernelAccess, KernelMemoryRegionHandle};

impl MemoryAccess for KernelAccess<'_> {
    type Mapping = KernelMapping;

    fn create_mapping(
        &self,
        location: Location,
        size: usize,
        allocation_strategy: AllocationStrategy,
    ) -> Result<Self::Mapping, CreateMappingError> {
        // For now, we only support eager allocation
        assert!(
            matches!(allocation_strategy, AllocationStrategy::Eager),
            "only eager allocation is supported"
        );

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

        // Allocate physical frames and map them
        // TODO: Optimize by using 2MiB and 1GiB frames when possible instead of only 4KiB frames
        let frames = PhysicalMemory::allocate_frames::<Size4KiB>(page_count)
            .ok_or(CreateMappingError::OutOfMemory)?;

        self.process
            .address_space()
            .map_range::<Size4KiB>(
                &*segment,
                frames.into_iter(),
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::NO_EXECUTE,
            )
            .map_err(|_| CreateMappingError::OutOfMemory)?;

        Ok(KernelMapping {
            addr: segment.start,
            size,
            segment,
            physical_frames: frames,
        })
    }
}

pub struct KernelMapping {
    addr: VirtAddr,
    size: usize,
    segment: OwnedSegment<'static>,
    physical_frames: x86_64::structures::paging::frame::PhysFrameRangeInclusive<Size4KiB>,
}

impl KernelMapping {
    /// Convert this mapping into a MemoryRegion handle that can be tracked by the process.
    pub fn into_region_handle(self) -> KernelMemoryRegionHandle {
        let addr = self
            .addr
            .as_ptr::<u8>()
            .try_into()
            .expect("kernel mapping should be located in user space");
        let size = self.size;

        let inner = MemoryRegion::Mapped(MappedMemoryRegion::new(
            self.segment,
            self.size,
            self.physical_frames,
        ));

        KernelMemoryRegionHandle { addr, size, inner }
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
