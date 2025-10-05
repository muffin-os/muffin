use kernel_syscall::UserspacePtr;
use kernel_syscall::access::{
    AllocationStrategy, CreateMappingError, Location, Mapping, MemoryAccess,
};
use kernel_virtual_memory::Segment;
use x86_64::VirtAddr;
use x86_64::structures::paging::{PageSize, Size4KiB};

use crate::UsizeExt;
use crate::mem::virt::VirtualMemoryAllocator;
use crate::syscall::access::KernelAccess;

impl MemoryAccess for KernelAccess<'_> {
    type Mapping = KernelMapping;

    fn create_mapping(
        &self,
        location: Location,
        size: usize,
        allocation_strategy: AllocationStrategy,
    ) -> Result<Self::Mapping, CreateMappingError> {
        let segment = if let Location::Fixed(addr) = location {
            let size = size.next_multiple_of(Size4KiB::SIZE as usize);
            self.process
                .vmm()
                .mark_as_reserved(Segment::new(
                    VirtAddr::from_ptr(addr.as_ptr()),
                    size.into_u64(),
                ))
                .map_err(|_| CreateMappingError::LocationAlreadyMapped)?
        } else {
            let page_count =
                size.next_multiple_of(Size4KiB::SIZE as usize) / Size4KiB::SIZE as usize;
            self.process
                .vmm()
                .reserve(page_count)
                .ok_or(CreateMappingError::OutOfMemory)?
        };
        todo!()
    }
}

pub struct KernelMapping {
    addr: VirtAddr,
    size: usize,
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
