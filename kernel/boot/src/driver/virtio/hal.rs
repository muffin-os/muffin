use alloc::boxed::Box;
use core::ptr::NonNull;

use kernel_pci::PciAddress;
use kernel_pci::config::ConfigurationAccess;
use kernel_virtual_memory::Segment;
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::pci::bus::{DeviceFunction, PciRoot};
use virtio_drivers::{BufferDirection, Hal};
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use crate::driver::pci::VirtIoCam;
use crate::mem::address_space::AddressSpace;
use crate::mem::phys::{OwnedPhysicalMemory, PhysicalMemory};
use crate::mem::virt::{VirtualMemoryAllocator, VirtualMemoryHigherHalf};
use crate::{U64Ext, UsizeExt};

pub fn transport(addr: PciAddress, cam: Box<dyn ConfigurationAccess>) -> PciTransport {
    let mut root = PciRoot::new(VirtIoCam::new(cam));
    PciTransport::new::<HalImpl, _>(
        &mut root,
        DeviceFunction {
            bus: addr.bus,
            device: addr.device,
            function: addr.function,
        },
    )
    .unwrap()
}

pub struct HalImpl;

unsafe impl Hal for HalImpl {
    fn dma_alloc(pages: usize, _: BufferDirection) -> (u64, NonNull<u8>) {
        let frames = PhysicalMemory::allocate_frames::<Size4KiB>(pages).unwrap();
        let start_address = frames.start.start_address().as_u64();
        let segment = VirtualMemoryHigherHalf.reserve(pages).unwrap();
        AddressSpace::kernel()
            .map_range::<Size4KiB>(
                &*segment,
                frames.leak(), // this has to be deallocated in dma_dealloc
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            )
            .unwrap();
        let segment = segment.leak();
        let addr = NonNull::new(segment.start.as_mut_ptr::<u8>()).unwrap();
        (start_address, addr)
    }

    unsafe fn dma_dealloc(paddr: u64, vaddr: NonNull<u8>, pages: usize) -> i32 {
        // strictly speaking this is not necessary, but since we allocate it through
        // an [`OwnedPhysicalMemory`] object, we should also deallocate it. That way,
        // for allocation and deallocation, everything goes through
        // [`OwnedPhysicalMemory`] which maybe makes debugging easier.
        //
        // Alternatively, we could only create the [`PhysFrameRangeInclusive`] and
        // then deallocate that directly.
        let frames = {
            let start = PhysFrame::containing_address(PhysAddr::new(paddr));
            let end = PhysFrame::containing_address(PhysAddr::new(
                paddr + (pages * Size4KiB::SIZE.into_usize()).into_u64() - 1,
            ));
            let range = PhysFrameRangeInclusive::<Size4KiB> { start, end };
            OwnedPhysicalMemory::from_physical_frame_range(range)
        };

        let segment = Segment::new(
            VirtAddr::from_ptr(vaddr.as_ptr()),
            pages.into_u64() * Size4KiB::SIZE,
        );
        unsafe {
            AddressSpace::kernel().unmap_range::<Size4KiB>(&segment, |_| {});
            assert!(VirtualMemoryHigherHalf.release(segment));
        }

        // we can not drop this any earlier, especially not until the respective virtual memory is unmapped
        drop(frames);

        0
    }

    unsafe fn mmio_phys_to_virt(paddr: u64, size: usize) -> NonNull<u8> {
        let frames = PhysFrameRangeInclusive::<Size4KiB> {
            start: PhysFrame::containing_address(PhysAddr::new(paddr)),
            end: PhysFrame::containing_address(PhysAddr::new(paddr + size.into_u64() - 1)),
        };

        let segment = VirtualMemoryHigherHalf
            .reserve(size.div_ceil(Size4KiB::SIZE.into_usize()))
            .unwrap();
        AddressSpace::kernel()
            .map_range::<Size4KiB>(
                &*segment,
                frames,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            )
            .unwrap();
        let segment = segment.leak();
        NonNull::new(segment.start.as_mut_ptr::<u8>()).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, _: BufferDirection) -> u64 {
        AddressSpace::kernel()
            .translate(VirtAddr::from_ptr(buffer.as_ptr()))
            .unwrap()
            .as_u64()
    }

    unsafe fn unshare(_: u64, _: NonNull<[u8]>, _: BufferDirection) {}
}
