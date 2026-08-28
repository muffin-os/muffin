use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::{mem, slice};

use kernel_vfs::node::VfsNode;
use kernel_virtual_memory::Segment;
use spin::mutex::Mutex;
use thiserror::Error;
use tracing::trace;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{Page, PageSize, PageTableFlags, Size4KiB};

use super::SoleLiveTask;
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
                MemoryRegion::Private(r) => r.map_zeroed(address_space, page)?,
                MemoryRegion::FileBacked(r) => r.page_in(address_space, page)?,
                MemoryRegion::Shared(_) => {}
            }
        }
        Ok(())
    }

    /// `_proof` guarantees no other task of the process is live, so no
    /// region can fault concurrently while it is torn down.
    pub fn clear(&self, address_space: &AddressSpace, _proof: &SoleLiveTask<'_>) {
        let regions = interrupts::without_interrupts(|| mem::take(&mut *self.regions.lock()));
        for region in regions {
            let size = region.size();
            if size == 0 {
                continue;
            }
            let start = region.addr();
            let end = start + (size - 1).into_u64();
            address_space.unmap_range::<Size4KiB>(
                Page::range_inclusive(
                    Page::containing_address(start),
                    Page::containing_address(end),
                ),
                |_| {},
            );
        }
    }
}

#[derive(Debug)]
pub enum MemoryRegion {
    /// A private mapping (anonymous or CoW-shared after fork). Pages are
    /// mapped in lazily by the page fault handler upon access.
    ///
    /// - [`PrivateMemoryRegion`]
    Private(PrivateMemoryRegion),
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
            MemoryRegion::Private(private_memory_region) => private_memory_region.start,
            MemoryRegion::FileBacked(file_backed_memory_region) => {
                file_backed_memory_region.region.segment().start
            }
            MemoryRegion::Shared(shared_memory_region) => shared_memory_region.segment.start,
        }
    }

    pub fn size(&self) -> usize {
        match self {
            MemoryRegion::Private(private_memory_region) => private_memory_region.size,
            MemoryRegion::FileBacked(file_backed_memory_region) => {
                file_backed_memory_region.region.size
            }
            MemoryRegion::Shared(shared_memory_region) => shared_memory_region.size,
        }
    }

    pub fn contains(&self, addr: VirtAddr) -> bool {
        // Forming `addr + size` as a VirtAddr panics for a region ending
        // exactly at the canonical lower-half boundary.
        addr.as_u64()
            .checked_sub(self.addr().as_u64())
            .is_some_and(|offset| offset < self.size().into_u64())
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
struct PrivateRegionState {
    /// The flags a privately owned page is mapped with. While a frame is
    /// shared (fork), pages are mapped with these flags minus WRITABLE.
    flags: PageTableFlags,
    /// Single-page frames from demand paging and CoW breaks.
    pages: BTreeMap<Page<Size4KiB>, Arc<OwnedPhysicalMemory>>,
}

#[derive(Debug)]
pub struct PrivateMemoryRegion {
    segment: OwnedSegment<'static>,
    /// The first accessible address. Equals `segment.start` except for
    /// guarded regions, where `segment` also reserves the surrounding guard
    /// pages.
    start: VirtAddr,
    /// The size of the region. This may differ from the
    /// size of the segment in that the size of the segment
    /// is page-aligned, while this may not be.
    ///
    /// For example, the segment of a memory region whose
    /// size is 5 bytes is actually 4096 bytes.
    size: usize,
    /// Serializes all backing mutations, all flags changes, and all fault
    /// resolution for this region.
    state: Mutex<PrivateRegionState>,
}

impl PrivateMemoryRegion {
    pub fn new(segment: OwnedSegment<'static>, size: usize, flags: PageTableFlags) -> Self {
        Self {
            start: segment.start,
            segment,
            size,
            state: Mutex::new(PrivateRegionState {
                flags,
                pages: BTreeMap::new(),
            }),
        }
    }

    pub fn segment(&self) -> &Segment {
        &self.segment
    }

    pub fn start(&self) -> VirtAddr {
        self.start
    }

    fn map_and_fill(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        fill: impl FnOnce(&mut [u8; 4096]) -> Result<(), PageInError>,
    ) -> Result<(), PageInError> {
        let mut state = self.state.lock();
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
            .remap::<Size4KiB, _>(page, |_| state.flags)
            .map_err(|_| PageInError::MapFailed)?;

        state.pages.insert(page, Arc::new(owned));
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
pub struct FileBackedMemoryRegion {
    region: PrivateMemoryRegion,
    node: VfsNode,
    file_offset: usize,
    file_len: usize,
}

impl FileBackedMemoryRegion {
    pub fn new(
        region: PrivateMemoryRegion,
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
        let off = (page.start_address() - self.region.start()).into_usize();
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
