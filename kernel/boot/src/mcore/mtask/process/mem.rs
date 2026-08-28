use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};
use core::{mem, slice};

use kernel_vfs::node::VfsNode;
use kernel_virtual_memory::Segment;
use spin::mutex::Mutex;
use thiserror::Error;
use tracing::trace;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{Page, PageSize, PageTableFlags, PhysFrame, Size4KiB};

use super::SoleLiveTask;
use crate::mem::address_space::AddressSpace;
use crate::mem::phys::{OwnedPhysicalMemory, PhysicalMemory};
use crate::mem::virt::{OwnedSegment, VirtualMemoryAllocator, VirtualMemoryHigherHalf};
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

    pub fn handle_fault(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        caused_by_write: bool,
    ) -> Result<(), PageInError> {
        match self {
            MemoryRegion::Private(r) => {
                r.handle_fault(address_space, page, caused_by_write, |buf| {
                    buf.fill(0);
                    Ok(())
                })
            }
            MemoryRegion::FileBacked(r) => r.handle_fault(address_space, page, caused_by_write),
            MemoryRegion::Shared(r) => r.handle_fault(address_space, page),
        }
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
    #[error("write to read-only region")]
    NotWritable,
}

/// Maps `frame` at a transient higher-half address of the active address
/// space and hands the byte view to `f`. The kernel-half L4 entries are
/// shared between all address spaces, so the mapping is process independent.
fn with_frame_mapped<R>(
    address_space: &AddressSpace,
    frame: PhysFrame<Size4KiB>,
    f: impl FnOnce(&mut [u8; 4096]) -> R,
) -> Result<R, PageInError> {
    let segment = VirtualMemoryHigherHalf
        .reserve(1)
        .ok_or(PageInError::OutOfMemory)?;
    let page = Page::<Size4KiB>::containing_address(segment.start);
    let _kernel_half = AddressSpace::lock_kernel_half();
    address_space
        .map(
            page,
            frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
        )
        .map_err(|_| PageInError::MapFailed)?;
    let buf = unsafe { &mut *segment.start.as_mut_ptr::<[u8; 4096]>() };
    let result = f(buf);
    address_space.unmap::<Size4KiB>(page);
    Ok(result)
}

#[derive(Debug)]
struct PrivateRegionState {
    /// The flags a privately owned page is mapped with. While a frame is
    /// shared (fork), pages are mapped with these flags minus WRITABLE.
    flags: PageTableFlags,
    /// Single-page frames from demand paging and CoW breaks.
    pages: BTreeMap<Page<Size4KiB>, Arc<OwnedPhysicalMemory>>,
    /// Whole contiguous eager allocations, keyed by their first page.
    /// Immutable, never split or merged (invariant 2), shadowed by `pages`.
    ranges: BTreeMap<Page<Size4KiB>, Arc<OwnedPhysicalMemory>>,
}

impl PrivateRegionState {
    fn backing(&self, page: Page<Size4KiB>) -> Option<(usize, PhysFrame<Size4KiB>)> {
        if let Some(backing) = self.pages.get(&page) {
            return Some((Arc::strong_count(backing), backing.start));
        }
        let (first, backing) = self.ranges.range(..=page).next_back()?;
        let offset = page - *first;
        let frames = backing.end - backing.start + 1;
        (offset < frames).then(|| (Arc::strong_count(backing), backing.start + offset))
    }

    /// The caller holds the region lock and has already checked that `page`
    /// is unmapped, so the map cannot collide.
    fn map_and_fill(
        &mut self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        fill: impl FnOnce(&mut [u8; 4096]) -> Result<(), PageInError>,
    ) -> Result<(), PageInError> {
        let frame = PhysicalMemory::allocate_frame::<Size4KiB>().ok_or(PageInError::OutOfMemory)?;
        let owned = OwnedPhysicalMemory::from_physical_frame(frame);

        address_space
            .map(
                page,
                frame,
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::NO_EXECUTE,
            )
            .map_err(|_| PageInError::MapFailed)?;

        let buf = unsafe { &mut *page.start_address().as_mut_ptr::<[u8; 4096]>() };
        if let Err(e) = fill(buf) {
            address_space.unmap::<Size4KiB>(page);
            return Err(e);
        }

        address_space
            .remap::<Size4KiB, _>(page, |_| self.flags)
            .map_err(|_| PageInError::MapFailed)?;

        self.pages.insert(page, Arc::new(owned));
        Ok(())
    }
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
                ranges: BTreeMap::new(),
            }),
        }
    }

    pub fn segment(&self) -> &Segment {
        &self.segment
    }

    pub fn start(&self) -> VirtAddr {
        self.start
    }

    /// `fill` provides the content of a page that has no backing yet. It
    /// runs under the region lock, which cannot deadlock against mount
    /// locks because faulting while a mount lock is held is forbidden (see
    /// `make_user_range_resident`).
    fn handle_fault(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        write: bool,
        fill: impl FnOnce(&mut [u8; 4096]) -> Result<(), PageInError>,
    ) -> Result<(), PageInError> {
        let mut state = self.state.lock();
        let flags = state.flags;
        match address_space.translate_flags(page.start_address()) {
            // Mapped already, so a racing task resolved the fault or a
            // stale TLB entry produced it. Retrying makes progress. #PF
            // delivery invalidates the TLB entries for the faulting address.
            Some(f) if !write || f.contains(PageTableFlags::WRITABLE) => Ok(()),
            Some(_) if !flags.contains(PageTableFlags::WRITABLE) => Err(PageInError::NotWritable),
            Some(_) => {
                let (count, _) = state.backing(page).ok_or(PageInError::MapFailed)?;
                if count == 1 {
                    // strong_count loads Relaxed. The fence pairs with the
                    // Release decrement in Arc::drop and orders the
                    // dropper's reads before writes through the writable
                    // mapping.
                    fence(Ordering::Acquire);
                    address_space
                        .remap::<Size4KiB, _>(page, |_| flags)
                        .map_err(|_| PageInError::MapFailed)?;
                } else {
                    let frame = PhysicalMemory::allocate_frame::<Size4KiB>()
                        .ok_or(PageInError::OutOfMemory)?;
                    let owned = OwnedPhysicalMemory::from_physical_frame(frame);
                    with_frame_mapped(address_space, frame, |dst| {
                        let src = unsafe { &*page.start_address().as_ptr::<[u8; 4096]>() };
                        dst.copy_from_slice(src);
                    })?;
                    address_space.unmap::<Size4KiB>(page);
                    address_space
                        .map(page, frame, flags)
                        .map_err(|_| PageInError::MapFailed)?;
                    // Publish only after the copy completed.
                    // A shadowed range frame stays retained until the range
                    // Arc drops.
                    state.pages.insert(page, Arc::new(owned));
                }
                Ok(())
            }
            None => match state.backing(page) {
                Some((count, frame)) => {
                    if !flags.contains(PageTableFlags::WRITABLE) {
                        if write {
                            return Err(PageInError::NotWritable);
                        }
                        address_space
                            .map(page, frame, flags)
                            .map_err(|_| PageInError::MapFailed)?;
                    } else if count == 1 {
                        // strong_count loads Relaxed. The fence pairs with
                        // the Release decrement in Arc::drop and orders the
                        // dropper's reads before writes through the
                        // writable mapping.
                        fence(Ordering::Acquire);
                        address_space
                            .map(page, frame, flags)
                            .map_err(|_| PageInError::MapFailed)?;
                    } else if !write {
                        address_space
                            .map(page, frame, flags - PageTableFlags::WRITABLE)
                            .map_err(|_| PageInError::MapFailed)?;
                    } else {
                        let new = PhysicalMemory::allocate_frame::<Size4KiB>()
                            .ok_or(PageInError::OutOfMemory)?;
                        let owned = OwnedPhysicalMemory::from_physical_frame(new);
                        address_space
                            .map(page, new, flags)
                            .map_err(|_| PageInError::MapFailed)?;
                        if let Err(e) = with_frame_mapped(address_space, frame, |src| {
                            let dst =
                                unsafe { &mut *page.start_address().as_mut_ptr::<[u8; 4096]>() };
                            dst.copy_from_slice(src);
                        }) {
                            // Never leave a mapping to a frame that
                            // `owned` frees.
                            address_space.unmap::<Size4KiB>(page);
                            return Err(e);
                        }
                        state.pages.insert(page, Arc::new(owned));
                    }
                    Ok(())
                }
                None => state.map_and_fill(address_space, page, fill),
            },
        }
    }

    fn map_and_fill_locked(
        &self,
        state: &mut PrivateRegionState,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        fill: impl FnOnce(&mut [u8; 4096]) -> Result<(), PageInError>,
    ) -> Result<(), PageInError> {
        let frame = PhysicalMemory::allocate_frame::<Size4KiB>().ok_or(PageInError::OutOfMemory)?;
        let owned = OwnedPhysicalMemory::from_physical_frame(frame);

        // PageAlreadyMapped is unreachable. The caller holds the region lock
        // and has already checked the page is unmapped, and no other path
        // maps pages of this region.
        address_space
            .map(
                page,
                frame,
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::NO_EXECUTE,
            )
            .map_err(|_| PageInError::MapFailed)?;

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

    fn map_zeroed(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
    ) -> Result<(), PageInError> {
        self.handle_fault(address_space, page, false, |buf| {
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

    fn handle_fault(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        write: bool,
    ) -> Result<(), PageInError> {
        let off = (page.start_address() - self.region.start()).into_usize();
        let from_file = self
            .file_len
            .saturating_sub(off)
            .min(Size4KiB::SIZE.into_usize());
        self.region
            .handle_fault(address_space, page, write, |buf| {
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

    fn page_in(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
    ) -> Result<(), PageInError> {
        self.handle_fault(address_space, page, false)
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
    /// Borrowed from the device. Never deallocated (see the struct docs).
    frames: PhysFrameRangeInclusive<Size4KiB>,
    _node: VfsNode,
}

impl SharedMemoryRegion {
    pub fn new(
        segment: OwnedSegment<'static>,
        size: usize,
        frames: PhysFrameRangeInclusive<Size4KiB>,
        node: VfsNode,
    ) -> Self {
        Self {
            segment,
            size,
            frames,
            _node: node,
        }
    }

    fn handle_fault(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
    ) -> Result<(), PageInError> {
        if address_space.translate(page.start_address()).is_some() {
            return Ok(());
        }
        let offset = (page.start_address() - self.segment.start) / Size4KiB::SIZE;
        address_space
            .map(
                page,
                self.frames.start + offset,
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::NO_EXECUTE,
            )
            .map_err(|_| PageInError::MapFailed)?;
        Ok(())
    }
}
