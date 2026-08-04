use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::slice;

use kernel_vfs::node::VfsNode;
use kernel_virtual_memory::Segment;
use spin::mutex::Mutex;
use thiserror::Error;
use tracing::{debug, trace};
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{Page, PageSize, PageTableFlags, Size4KiB};

use crate::mem::address_space::AddressSpace;
use crate::mem::phys::{OwnedPhysicalMemory, PhysicalMemory};
use crate::mem::virt::OwnedSegment;
use crate::{U64Ext, UsizeExt};

/// Tracks a process's virtual memory regions, including the lazily mapped ones
/// the page fault handler resolves.
pub struct MemoryRegions {
    regions: Mutex<Vec<Arc<MemoryRegion>>>,
}

impl Default for MemoryRegions {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryRegions {
    pub fn new() -> Self {
        Self {
            regions: Mutex::new(vec![]),
        }
    }

    pub fn add_region(&self, region: MemoryRegion) {
        interrupts::without_interrupts(|| self.regions.lock().push(Arc::new(region)));
    }

    pub fn region_for(&self, addr: VirtAddr) -> Option<Arc<MemoryRegion>> {
        interrupts::without_interrupts(|| {
            self.regions
                .lock()
                .iter()
                .find(|r| r.contains(addr))
                .cloned()
        })
    }

    pub fn is_memory_region_at_address(&self, addr: VirtAddr) -> bool {
        interrupts::without_interrupts(|| self.regions.lock().iter().any(|r| r.contains(addr)))
    }

    pub fn populate(
        &self,
        address_space: &AddressSpace,
        addr: VirtAddr,
        len: usize,
    ) -> Result<(), PageInError> {
        let Some(last) = len
            .checked_sub(1)
            .and_then(|l| addr.as_u64().checked_add(l.into_u64()))
        else {
            return Ok(());
        };
        let Ok(end) = VirtAddr::try_new(last) else {
            return Ok(());
        };

        for page in Page::<Size4KiB>::range_inclusive(
            Page::containing_address(addr),
            Page::containing_address(end),
        ) {
            if address_space.translate(page.start_address()).is_some() {
                continue;
            }
            let Some(region) = self.region_for(page.start_address()) else {
                continue;
            };
            match &*region {
                MemoryRegion::Lazy(r) => r.map_zeroed(address_space, page)?,
                MemoryRegion::FileBacked(r) => r.page_in(address_space, page)?,
                MemoryRegion::Mapped(_) | MemoryRegion::Shared(_) => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum MemoryRegion {
    /// A memory region that will have its memory mapped in lazily
    /// by the page fault handler upon access to a page.
    ///
    /// - [`LazyMemoryRegion`]
    Lazy(LazyMemoryRegion),
    /// A memory region whose entire memory is already mapped.
    /// One could call it a "normal piece of memory".
    ///
    /// - [`MappedMemoryRegion`]
    Mapped(MappedMemoryRegion),
    /// A memory region that is lazy, but is additionally backed by
    /// a file. The page handler will map the pages lazily upon access,
    /// and read the bytes from the respective location from the backing
    /// file.
    ///
    /// - [`FileBackedMemoryRegion`]
    FileBacked(FileBackedMemoryRegion),
    /// A memory region backed by a device file's physical frames, mapped
    /// as a shared mapping. It owns the virtual reservation and keeps the
    /// device file open.
    ///
    /// - [`SharedMemoryRegion`]
    Shared(SharedMemoryRegion),
}

impl MemoryRegion {
    pub fn addr(&self) -> VirtAddr {
        match self {
            MemoryRegion::Lazy(lazy_memory_region) => lazy_memory_region.segment.start,
            MemoryRegion::Mapped(mapped_memory_region) => mapped_memory_region.segment.start,
            MemoryRegion::FileBacked(file_backed_memory_region) => {
                file_backed_memory_region.region.segment().start
            }
            MemoryRegion::Shared(shared_memory_region) => shared_memory_region.segment.start,
        }
    }

    pub fn size(&self) -> usize {
        match self {
            MemoryRegion::Lazy(lazy_memory_region) => lazy_memory_region.size,
            MemoryRegion::Mapped(mapped_memory_region) => mapped_memory_region.size,
            MemoryRegion::FileBacked(file_backed_memory_region) => {
                file_backed_memory_region.region.size
            }
            MemoryRegion::Shared(shared_memory_region) => shared_memory_region.size,
        }
    }

    pub fn contains(&self, addr: VirtAddr) -> bool {
        self.addr() <= addr && self.addr() + self.size().into_u64() > addr
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.addr().as_ptr(), self.size()) }
    }
}

#[derive(Debug, Error)]
pub enum PageInError {
    #[error("out of physical memory")]
    OutOfMemory,
    #[error("failed to map page")]
    MapFailed,
    #[error("failed to read backing file")]
    ReadFailed,
}

#[derive(Debug)]
pub struct LazyMemoryRegion {
    segment: OwnedSegment<'static>,
    /// The size of the region. This may differ from the
    /// size of the segment in that the size of the segment
    /// is page-aligned, while this may not be.
    ///
    /// For example, the segment of a memory region whose
    /// size is 5 bytes is actually 4096 bytes.
    size: usize,
    flags: PageTableFlags,
    /// The physical frames that were mapped for this lazy
    /// memory region.
    physical_frames: Mutex<Vec<OwnedPhysicalMemory>>,
}

impl LazyMemoryRegion {
    pub fn new(segment: OwnedSegment<'static>, size: usize, flags: PageTableFlags) -> Self {
        Self {
            segment,
            size,
            flags,
            physical_frames: Mutex::new(vec![]),
        }
    }

    pub fn segment(&self) -> &Segment {
        &self.segment
    }

    fn map_and_fill(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        fill: impl FnOnce(&mut [u8; 4096]) -> Result<(), PageInError>,
    ) -> Result<(), PageInError> {
        let frame = PhysicalMemory::allocate_frame::<Size4KiB>().ok_or(PageInError::OutOfMemory)?;
        let owned = OwnedPhysicalMemory::from_physical_frame(frame);

        match address_space.map(
            page,
            frame,
            PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE
                | PageTableFlags::NO_EXECUTE,
        ) {
            Ok(()) => {}
            Err(MapToError::PageAlreadyMapped(_)) => return Ok(()),
            Err(_) => return Err(PageInError::MapFailed),
        }

        let buf = unsafe { &mut *page.start_address().as_mut_ptr::<[u8; 4096]>() };
        if let Err(e) = fill(buf) {
            address_space.unmap::<Size4KiB>(page);
            return Err(e);
        }

        address_space
            .remap::<Size4KiB, _>(page, |_| self.flags)
            .map_err(|_| PageInError::MapFailed)?;

        self.physical_frames.lock().push(owned);
        Ok(())
    }

    pub fn map_zeroed(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
    ) -> Result<(), PageInError> {
        self.map_and_fill(address_space, page, |buf| {
            buf.fill(0);
            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct MappedMemoryRegion {
    segment: OwnedSegment<'static>,
    size: usize,
    _physical_memory: OwnedPhysicalMemory,
}

impl MappedMemoryRegion {
    pub fn new(
        segment: OwnedSegment<'static>,
        size: usize,
        physical_memory: OwnedPhysicalMemory,
    ) -> Self {
        Self {
            segment,
            size,
            _physical_memory: physical_memory,
        }
    }
}

#[derive(Debug)]
pub struct FileBackedMemoryRegion {
    region: LazyMemoryRegion,
    node: VfsNode,
    file_offset: usize,
    file_len: usize,
}

impl FileBackedMemoryRegion {
    pub fn new(
        region: LazyMemoryRegion,
        node: VfsNode,
        file_offset: usize,
        file_len: usize,
    ) -> Self {
        Self {
            region,
            node,
            file_offset,
            file_len,
        }
    }

    pub fn page_in(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
    ) -> Result<(), PageInError> {
        let off = (page.start_address() - self.region.segment().start).into_usize();
        let from_file = self
            .file_len
            .saturating_sub(off)
            .min(Size4KiB::SIZE.into_usize());
        self.region.map_and_fill(address_space, page, |buf| {
            buf[from_file..].fill(0);
            let mut done = 0;
            while done < from_file {
                match self
                    .node
                    .read(&mut buf[done..from_file], self.file_offset + off + done)
                {
                    Ok(0) => break,
                    Ok(n) => done += n,
                    Err(_) => return Err(PageInError::ReadFailed),
                }
            }
            buf[done..from_file].fill(0);
            Ok(())
        })?;
        trace!(page = ?page.start_address(), "paged in");
        Ok(())
    }
}

/// Owns the virtual reservation for a device-backed shared mapping and keeps
/// the backing device file open.
///
/// It deliberately holds no [`OwnedPhysicalMemory`]. The device owns those
/// frames, so dropping this region must release only the virtual range and
/// never the frames. Deallocating them would hand live device memory back to
/// the frame allocator and corrupt the device.
#[derive(Debug)]
pub struct SharedMemoryRegion {
    segment: OwnedSegment<'static>,
    size: usize,
    _node: VfsNode,
}

impl SharedMemoryRegion {
    pub fn new(segment: OwnedSegment<'static>, size: usize, node: VfsNode) -> Self {
        Self {
            segment,
            size,
            _node: node,
        }
    }
}
