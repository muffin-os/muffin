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
use crate::mem::phys::PhysicalMemory;
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
    fn dma_alloc(pages: usize, _: BufferDirection) -> (usize, NonNull<u8>) {
        let frames = PhysicalMemory::allocate_frames(pages).unwrap();
        let segment = VirtualMemoryHigherHalf.reserve(pages).unwrap();
        AddressSpace::kernel()
            .map_range::<Size4KiB>(
                &*segment,
                frames,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            )
            .unwrap();
        let segment = segment.leak();
        let addr = NonNull::new(segment.start.as_mut_ptr::<u8>()).unwrap();
        (frames.start.start_address().as_u64().into_usize(), addr)
    }

    unsafe fn dma_dealloc(paddr: usize, vaddr: NonNull<u8>, pages: usize) -> i32 {
        let frames = PhysFrameRangeInclusive::<Size4KiB> {
            start: PhysFrame::containing_address(PhysAddr::new(paddr.into_u64())),
            end: PhysFrame::containing_address(PhysAddr::new(
                (paddr + (pages * Size4KiB::SIZE.into_usize()) - 1).into_u64(),
            )),
        };
        let segment = Segment::new(
            VirtAddr::from_ptr(vaddr.as_ptr()),
            pages.into_u64() * Size4KiB::SIZE,
        );
        unsafe {
            AddressSpace::kernel().unmap_range::<Size4KiB>(&segment, |_| {});
            assert!(VirtualMemoryHigherHalf.release(segment));
            PhysicalMemory::deallocate_frames(frames);
        }

        0
    }

    unsafe fn mmio_phys_to_virt(paddr: usize, size: usize) -> NonNull<u8> {
        let frames = PhysFrameRangeInclusive::<Size4KiB> {
            start: PhysFrame::containing_address(PhysAddr::new(paddr.into_u64())),
            end: PhysFrame::containing_address(PhysAddr::new((paddr + size - 1).into_u64())),
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

    unsafe fn share(buffer: NonNull<[u8]>, _: BufferDirection) -> usize {
        AddressSpace::kernel()
            .translate(VirtAddr::from_ptr(buffer.as_ptr()))
            .unwrap()
            .as_u64()
            .into_usize()
    }

    unsafe fn unshare(_: usize, _: NonNull<[u8]>, _: BufferDirection) {}
}
