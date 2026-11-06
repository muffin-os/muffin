use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::{Ordering, fence};
use core::{mem, slice};

use kernel_vfs::MmapRegion;
use kernel_vfs::node::VfsNode;
use kernel_virtual_memory::{AlreadyReserved, Segment};
use spin::mutex::Mutex;
use thiserror::Error;
use tracing::trace;
use x86_64::VirtAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::mapper::FlagUpdateError;
use x86_64::structures::paging::{Page, PageSize, PageTableFlags, PhysFrame, Size4KiB};

use super::SoleLiveTask;
use crate::mem::address_space::AddressSpace;
use crate::mem::phys::{OwnedPhysicalMemory, PhysicalMemory};
use crate::mem::virt::{OwnedSegment, VirtualMemoryAllocator, VirtualMemoryHigherHalf};
use crate::sync::IrqMutex;
use crate::{U64Ext, UsizeExt};

/// Tracks a process's virtual memory regions, including the lazily mapped ones
/// the page fault handler resolves.
pub struct MemoryRegions {
    regions: IrqMutex<Vec<Arc<MemoryRegion>>>,
}

impl Default for MemoryRegions {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryRegions {
    pub fn new() -> Self {
        Self {
            regions: IrqMutex::new(vec![]),
        }
    }

    pub fn add_region(&self, region: MemoryRegion) -> Arc<MemoryRegion> {
        let region = Arc::new(region);
        self.regions.lock_without_interrupts().push(region.clone());
        region
    }

    /// This function removes `region` by pointer identity, unmaps its pages,
    /// and drops the TLB entries of every CPU for them. The frames stay
    /// allocated until the caller drops its `Arc`. That drop comes after the
    /// flush.
    ///
    /// # Panics
    /// This function panics if interrupts are off. In debug builds, it also
    /// panics if `address_space` is not active on this CPU.
    pub fn remove_region(&self, address_space: &AddressSpace, region: &Arc<MemoryRegion>) {
        debug_assert!(address_space.is_active());
        self.regions
            .lock_without_interrupts()
            .retain(|r| !Arc::ptr_eq(r, region));
        region.unmap_from(address_space);
        // See `AddressSpace::flush_on_all_cpus` for the lock rule.
        address_space.flush_on_all_cpus();
    }

    /// This function splits each region that `range` covers only in part at
    /// the bounds of `range`. It returns the regions that lie fully inside
    /// `range`. The bounds of `range` are page aligned addresses.
    ///
    /// The regions to split are absent from the list while they split. The
    /// caller holds the exclusive user memory guard. Therefore no fault and
    /// no syscall of the process looks for these regions during that time.
    ///
    /// # Errors
    /// This function returns the [`SplitError`] of the first region that
    /// cannot split. The regions that split before the failure stay split,
    /// and this changes no mapping. It returns `InUse` when a kernel handle
    /// of a region inside `range` exists.
    pub fn carve(&self, range: &Range<u64>) -> Result<Vec<Arc<MemoryRegion>>, SplitError> {
        let partial: Vec<Arc<MemoryRegion>> = {
            let mut regions = self.regions.lock_without_interrupts();
            let (partial, kept) = mem::take(&mut *regions)
                .into_iter()
                .partition(|r| r.overlaps(range) && !r.lies_inside(range));
            *regions = kept;
            partial
        };

        let mut first_error = None;
        for region in partial {
            if first_error.is_some() {
                self.regions.lock_without_interrupts().push(region);
                continue;
            }
            match MemoryRegion::split_at_bounds(region, range) {
                Ok(parts) => {
                    for part in parts {
                        self.add_region(part);
                    }
                }
                Err((region, error)) => {
                    first_error = Some(error);
                    self.regions.lock_without_interrupts().push(region);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }

        let regions = self.regions.lock_without_interrupts();
        let inside = regions.iter().filter(|r| r.lies_inside(range));
        // The list holds one `Arc`. Any other `Arc` is a kernel handle. An
        // example is the allocation of a TLS block, whose memory the kernel
        // uses.
        if inside.clone().any(|r| Arc::strong_count(r) > 1) {
            return Err(SplitError::InUse);
        }
        Ok(inside.cloned().collect())
    }

    /// This function returns true when the regions cover each address of `range`.
    pub fn covers(&self, range: &Range<u64>) -> bool {
        let mut spans: Vec<Range<u64>> = self
            .regions
            .lock_without_interrupts()
            .iter()
            .filter(|r| r.overlaps(range))
            .map(|r| r.span())
            .collect();
        spans.sort_unstable_by_key(|span| span.start);
        let mut covered_until = range.start;
        for span in spans {
            if span.start > covered_until {
                return false;
            }
            covered_until = covered_until.max(span.end);
        }
        covered_until >= range.end
    }

    pub fn region_for(&self, addr: VirtAddr) -> Option<Arc<MemoryRegion>> {
        self.regions
            .lock_without_interrupts()
            .iter()
            .find(|r| r.contains(addr))
            .cloned()
    }

    pub fn is_memory_region_at_address(&self, addr: VirtAddr) -> bool {
        self.regions
            .lock_without_interrupts()
            .iter()
            .any(|r| r.contains(addr))
    }

    pub fn populate(
        &self,
        address_space: &AddressSpace,
        addr: VirtAddr,
        len: usize,
        caused_by_write: bool,
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
            if let Some(flags) = address_space.translate_flags(page.start_address())
                && (!caused_by_write || flags.contains(PageTableFlags::WRITABLE))
            {
                continue;
            }
            let Some(region) = self.region_for(page.start_address()) else {
                continue;
            };
            region.handle_fault(address_space, page, caused_by_write)?;
        }
        Ok(())
    }

    /// Adds a counterpart of every region of this process to `target`. The
    /// counterparts share the frames copy-on-write. See
    /// [`MemoryRegion::clone_into`]. `CopyEager` regions get private copies;
    /// the counterpart's pages are unmapped until `populate_eager_copies`
    /// runs in the target address space.
    ///
    /// # Errors
    /// Returns the first error of [`MemoryRegion::clone_into`]. `target`
    /// keeps the regions cloned before the failure.
    ///
    /// # Panics
    /// In debug builds, panics if `address_space` is not active on this CPU.
    pub fn clone_into(
        &self,
        address_space: &AddressSpace,
        target: &MemoryRegions,
        target_vmm: &impl VirtualMemoryAllocator,
    ) -> Result<(), CloneRegionError> {
        debug_assert!(address_space.is_active());
        // A CPU that waits for the regions lock has interrupts off. The
        // snapshot keeps the shootdown outside the lock.
        let regions = self.regions.lock_without_interrupts().clone();
        for region in regions {
            if region.is_per_thread() {
                continue;
            }
            target.add_region(region.clone_into(address_space, target_vmm)?);
        }
        Ok(())
    }

    /// Maps every eager copy in the active address space. The child of a
    /// fork calls it before its first user instruction, because the
    /// scheduler and signal delivery write these pages with interrupts off
    /// and cannot page them in.
    ///
    /// # Errors
    /// Returns the first error of [`MemoryRegion::handle_fault`].
    pub fn populate_eager_copies(&self, address_space: &AddressSpace) -> Result<(), PageInError> {
        let regions = self.regions.lock_without_interrupts().clone();
        for region in regions {
            if let MemoryRegion::Private(r) = &*region
                && r.fork == ForkPolicy::CopyEager
            {
                self.populate(address_space, r.start, r.size, true)?;
            }
        }
        Ok(())
    }

    /// `_proof` guarantees no other task of the process is live, so no
    /// region can fault concurrently while it is torn down.
    pub fn clear(&self, address_space: &AddressSpace, _proof: &SoleLiveTask<'_>) {
        let regions = mem::take(&mut *self.regions.lock_without_interrupts());
        for region in regions {
            region.unmap_from(address_space);
        }
    }
}

#[derive(Debug)]
pub enum MemoryRegion {
    /// A private mapping. Pages are mapped in lazily by the page fault
    /// handler upon access.
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
    /// The first user-addressable byte, after any guard page.
    pub fn addr(&self) -> VirtAddr {
        match self {
            MemoryRegion::Private(r) => r.start(),
            MemoryRegion::FileBacked(r) => r.start(),
            MemoryRegion::Shared(r) => r.start(),
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

    /// This function returns the addresses of the region as a half-open range.
    fn span(&self) -> Range<u64> {
        let start = self.addr().as_u64();
        start..start + self.size().into_u64()
    }

    fn overlaps(&self, range: &Range<u64>) -> bool {
        let span = self.span();
        !(span.end <= range.start || span.start >= range.end)
    }

    fn lies_inside(&self, range: &Range<u64>) -> bool {
        let span = self.span();
        range.start <= span.start && span.end <= range.end
    }

    /// This function returns true when the region is the TLS block, the FX
    /// area or the main user stack of one thread. A fork copies these regions
    /// only for the calling thread.
    pub fn is_per_thread(&self) -> bool {
        match self {
            MemoryRegion::Private(r) => r.per_thread,
            MemoryRegion::FileBacked(r) => r.region.per_thread,
            MemoryRegion::Shared(_) => false,
        }
    }

    /// This function splits the region at each bound of `range` that lies
    /// strictly inside the region. It returns the parts in address order.
    ///
    /// # Errors
    /// This function returns the unchanged region together with an error.
    /// The error comes from [`MemoryRegion::check_split`], or it is `InUse`
    /// when another `Arc` of the region exists.
    fn split_at_bounds(
        this: Arc<Self>,
        range: &Range<u64>,
    ) -> Result<Vec<Self>, (Arc<Self>, SplitError)> {
        if let Err(error) = this.check_split() {
            return Err((this, error));
        }
        let mut rest = Arc::try_unwrap(this).map_err(|this| (this, SplitError::InUse))?;
        let mut parts = Vec::new();
        for bound in [range.start, range.end] {
            if bound > rest.span().start && rest.span().contains(&bound) {
                let (low, high) = rest.split(VirtAddr::new(bound));
                parts.push(low);
                rest = high;
            }
        }
        parts.push(rest);
        Ok(parts)
    }

    /// # Errors
    /// This function returns `Shared` for a shared mapping. It returns
    /// `EagerBacking` for a region with a contiguous kernel allocation,
    /// which is never split.
    fn check_split(&self) -> Result<(), SplitError> {
        match self {
            MemoryRegion::Private(r) => r.check_split(),
            MemoryRegion::FileBacked(r) => r.region.check_split(),
            MemoryRegion::Shared(_) => Err(SplitError::Shared),
        }
    }

    /// This function splits the region at `at` into the part below `at` and
    /// the part from `at` on. Each part keeps the flags, the fork policy and
    /// the pages of its own addresses.
    ///
    /// # Panics
    /// This function panics when [`MemoryRegion::check_split`] fails. It also
    /// panics when `at` is not a page boundary strictly inside the region.
    fn split(self, at: VirtAddr) -> (Self, Self) {
        match self {
            MemoryRegion::Private(r) => {
                let (low, high) = r.split(at);
                (MemoryRegion::Private(low), MemoryRegion::Private(high))
            }
            MemoryRegion::FileBacked(r) => {
                let (low, high) = r.split(at);
                (
                    MemoryRegion::FileBacked(low),
                    MemoryRegion::FileBacked(high),
                )
            }
            MemoryRegion::Shared(_) => panic!("a shared mapping is never split"),
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.addr().as_ptr(), self.size()) }
    }

    fn unmap_from(&self, address_space: &AddressSpace) {
        match self {
            MemoryRegion::Private(r) => r.unmap_from(address_space),
            MemoryRegion::FileBacked(r) => r.region.unmap_from(address_space),
            MemoryRegion::Shared(r) => {
                let Some(last) = r.size.checked_sub(1) else {
                    return;
                };
                address_space.unmap_range::<Size4KiB>(
                    Page::range_inclusive(
                        Page::containing_address(r.start()),
                        Page::containing_address(r.start() + last.into_u64()),
                    ),
                    |_| {},
                );
            }
        }
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

    /// Replaces the logical flags of the region and remaps its currently
    /// mapped pages in the active address space.
    pub fn set_protection(
        &self,
        address_space: &AddressSpace,
        new_flags: PageTableFlags,
    ) -> Result<(), SetProtectionError> {
        match self {
            MemoryRegion::Private(r) => r.set_protection(address_space, new_flags),
            MemoryRegion::FileBacked(r) => r.region.set_protection(address_space, new_flags),
            MemoryRegion::Shared(_) => Err(SetProtectionError::UnsupportedRegion),
        }
    }

    /// Creates the counterpart of this region for another process. Private
    /// and file-backed regions share their frames copy-on-write. Shared
    /// regions share the device frames as they are.
    ///
    /// # Errors
    /// Returns `SegmentReserved` when `target_vmm` already holds the region's
    /// address range. Returns `RemapFailed` when a resident page of a private
    /// or file-backed region cannot become read-only.
    pub fn clone_into(
        &self,
        address_space: &AddressSpace,
        target_vmm: &impl VirtualMemoryAllocator,
    ) -> Result<MemoryRegion, CloneRegionError> {
        match self {
            MemoryRegion::Private(r) => Ok(MemoryRegion::Private(
                r.clone_into(address_space, target_vmm)?,
            )),
            MemoryRegion::FileBacked(r) => Ok(MemoryRegion::FileBacked(FileBackedMemoryRegion {
                region: r.region.clone_into(address_space, target_vmm)?,
                node: r.node.clone(),
                file_offset: r.file_offset,
                file_len: r.file_len,
            })),
            MemoryRegion::Shared(r) => Ok(MemoryRegion::Shared(SharedMemoryRegion {
                segment: target_vmm
                    .mark_as_reserved(*r.segment)
                    .map_err(|AlreadyReserved| CloneRegionError::SegmentReserved)?,
                size: r.size,
                frames: r.frames,
                memory: r.memory.clone(),
            })),
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
    #[error("access to a region without user access")]
    NotAccessible,
}

#[derive(Debug, Error)]
pub enum SetProtectionError {
    #[error("region does not support protection changes")]
    UnsupportedRegion,
    #[error("failed to remap a mapped page")]
    RemapFailed,
}

#[derive(Debug, Error)]
pub enum SplitError {
    #[error("a contiguous kernel allocation is never split")]
    EagerBacking,
    #[error("a shared mapping is never split")]
    Shared,
    #[error("a kernel handle of the region exists")]
    InUse,
}

#[derive(Debug, Error)]
pub enum CloneRegionError {
    #[error("the region's address range is already reserved in the target")]
    SegmentReserved,
    #[error("failed to make a shared page read-only")]
    RemapFailed,
    #[error("out of physical memory")]
    OutOfMemory,
    #[error("failed to map a copied page")]
    MapFailed,
}

/// This function maps `frame` at a transient higher-half address of the
/// active address space and gives the byte view to `f`. All address spaces
/// share the kernel-half L4 entries, so the mapping does not depend on the
/// process. `f` runs without the kernel-half lock. Therefore `f` can
/// allocate kernel memory and read from a file.
fn with_frame_mapped<R>(
    address_space: &AddressSpace,
    frame: PhysFrame<Size4KiB>,
    f: impl FnOnce(&mut [u8; 4096]) -> R,
) -> Result<R, PageInError> {
    let segment = VirtualMemoryHigherHalf
        .reserve(1)
        .ok_or(PageInError::OutOfMemory)?;
    let page = Page::<Size4KiB>::containing_address(segment.start);
    {
        let _kernel_half = AddressSpace::lock_kernel_half();
        address_space
            .map(
                page,
                frame,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            )
            .map_err(|_| PageInError::MapFailed)?;
    }
    let buf = unsafe { &mut *segment.start.as_mut_ptr::<[u8; 4096]>() };
    let result = f(buf);
    let _kernel_half = AddressSpace::lock_kernel_half();
    address_space.unmap::<Size4KiB>(page);
    Ok(result)
}

#[derive(Debug)]
struct PrivateRegionState {
    /// The flags a privately owned page is mapped with. While a frame is
    /// shared, pages are mapped with these flags minus WRITABLE.
    flags: PageTableFlags,
    /// Single-page frames from demand paging and copy-on-write breaks.
    pages: BTreeMap<Page<Size4KiB>, Arc<OwnedPhysicalMemory>>,
    /// Whole contiguous eager allocations, keyed by their first page.
    /// Immutable, never split or merged.
    ranges: BTreeMap<Page<Size4KiB>, Arc<OwnedPhysicalMemory>>,
}

impl PrivateRegionState {
    fn backing(&self, page: Page<Size4KiB>) -> Option<(usize, PhysFrame<Size4KiB>)> {
        if let Some(backing) = self.pages.get(&page) {
            return Some((Arc::strong_count(backing), backing.start));
        }
        let (first, backing) = self.ranges.range(..=page).next_back()?;
        let offset = page - *first;
        (offset < backing.len()).then(|| (Arc::strong_count(backing), backing.start + offset))
    }

    /// This function maps `page` and fills it. The caller holds the region
    /// lock. `page` has no mapping at this point, so the map cannot collide.
    /// The page becomes visible only with its final content and flags.
    /// Another thread of the process can touch the page on another CPU at any
    /// time. A page that is present but not yet executable kills that thread
    /// when the thread executes code on it.
    fn map_and_fill(
        &mut self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        fill: impl FnOnce(&mut [u8; 4096]) -> Result<(), PageInError>,
    ) -> Result<(), PageInError> {
        let frame = PhysicalMemory::allocate_frame::<Size4KiB>().ok_or(PageInError::OutOfMemory)?;
        let owned = OwnedPhysicalMemory::from_physical_frame(frame);
        with_frame_mapped(address_space, frame, fill)??;
        address_space
            .map(page, frame, self.flags)
            .map_err(|_| PageInError::MapFailed)?;
        self.pages.insert(page, Arc::new(owned));
        Ok(())
    }
}

/// How `clone_into` treats the region's resident pages.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ForkPolicy {
    /// Share the frames and demote writable pages to read-only.
    CopyOnWrite,
    /// Copy the pages into private frames. The region's pages stay as they are.
    CopyEager,
}

#[derive(Debug)]
pub struct PrivateMemoryRegion {
    /// Held only to release the virtual reservation with the region.
    _segment: OwnedSegment<'static>,
    /// The first accessible address. Equals the reservation's start except
    /// for guarded regions, where the reservation also covers the
    /// surrounding guard pages.
    start: VirtAddr,
    /// The accessible size in bytes starting at `start`. It is not
    /// necessarily page aligned. The reservation can exceed it, because it
    /// is page aligned and for guarded regions also covers the surrounding
    /// guard pages.
    size: usize,
    fork: ForkPolicy,
    /// See [`MemoryRegion::is_per_thread`].
    per_thread: bool,
    /// Serializes all backing mutations, all flags changes, and all fault
    /// resolution for this region.
    state: Mutex<PrivateRegionState>,
}

impl PrivateMemoryRegion {
    pub fn new(segment: OwnedSegment<'static>, size: usize, flags: PageTableFlags) -> Self {
        Self {
            start: segment.start,
            _segment: segment,
            size,
            fork: ForkPolicy::CopyOnWrite,
            per_thread: false,
            state: Mutex::new(PrivateRegionState {
                flags,
                pages: BTreeMap::new(),
                ranges: BTreeMap::new(),
            }),
        }
    }

    /// Creates a region and eagerly populates `page_count` pages starting
    /// at `start` in the active address space with zeroed pages. `flags`
    /// must include WRITABLE, because population writes the zeroes through
    /// the final mapping. On failure the whole window is unmapped before
    /// the backing frames are freed with the region.
    pub fn new_populated(
        segment: OwnedSegment<'static>,
        start: VirtAddr,
        page_count: usize,
        flags: PageTableFlags,
        fork: ForkPolicy,
        per_thread: bool,
        address_space: &AddressSpace,
    ) -> Result<Self, PageInError> {
        debug_assert!(flags.contains(PageTableFlags::WRITABLE));

        let size = page_count * Size4KiB::SIZE.into_usize();

        let region = Self {
            start,
            _segment: segment,
            size,
            fork,
            per_thread,
            state: Mutex::new(PrivateRegionState {
                flags,
                pages: BTreeMap::new(),
                ranges: BTreeMap::new(),
            }),
        };
        let first = Page::<Size4KiB>::containing_address(start);
        let result = {
            let mut state = region.state.lock();
            if let Some(owned) = PhysicalMemory::allocate_frames::<Size4KiB>(page_count) {
                let window = Segment::new(start, size.into_u64());
                match address_space.map_range::<Size4KiB>(&window, *owned, flags) {
                    Ok(()) => {
                        unsafe { core::ptr::write_bytes(start.as_mut_ptr::<u8>(), 0, size) };
                        state.ranges.insert(first, Arc::new(owned));
                        Ok(())
                    }
                    Err(_) => Err(PageInError::MapFailed),
                }
            } else {
                (0..page_count.into_u64()).try_for_each(|i| {
                    state.map_and_fill(address_space, first + i, |buf| {
                        buf.fill(0);
                        Ok(())
                    })
                })
            }
        };
        if let Err(e) = result {
            // The backing frames drop with `region`, so no page of the
            // window may still point at them.
            address_space.unmap_range::<Size4KiB>(
                Page::range_inclusive(first, first + (page_count.into_u64() - 1)),
                |_| {},
            );
            return Err(e);
        }
        Ok(region)
    }

    /// This function replaces the logical flags and remaps every currently
    /// mapped page. Pages governed by shared backings get the new flags
    /// minus WRITABLE. The fault handler handles unmapped backings later.
    /// When a mapped page loses write access or user access, every other CPU
    /// drops its TLB entries for this address space.
    ///
    /// # Panics
    /// This function panics if a mapped page loses write access or user
    /// access while interrupts are off.
    fn set_protection(
        &self,
        address_space: &AddressSpace,
        new_flags: PageTableFlags,
    ) -> Result<(), SetProtectionError> {
        let mut demoted = false;
        let mut remap = |page: Page<Size4KiB>, shared: bool| {
            let flags = if shared {
                new_flags - PageTableFlags::WRITABLE
            } else {
                new_flags
            };
            match address_space.remap::<Size4KiB, _>(page, move |_| flags) {
                Ok(()) => {
                    demoted |= !flags.contains(PageTableFlags::WRITABLE)
                        || !flags.contains(PageTableFlags::USER_ACCESSIBLE);
                    Ok(())
                }
                Err(FlagUpdateError::PageNotMapped) => Ok(()),
                Err(_) => Err(SetProtectionError::RemapFailed),
            }
        };

        let mut state = self.state.lock();
        state.flags = new_flags;
        // Ranges first and pages second, so the more specific shadow entry
        // applies its flags last.
        let result = state
            .ranges
            .iter()
            .try_for_each(|(first, backing)| {
                let shared = Arc::strong_count(backing) > 1;
                (0..backing.len()).try_for_each(|i| remap(*first + i, shared))
            })
            .and_then(|()| {
                state
                    .pages
                    .iter()
                    .try_for_each(|(page, backing)| remap(*page, Arc::strong_count(backing) > 1))
            });
        // See `AddressSpace::flush_on_all_cpus` for the lock rule.
        drop(state);
        if demoted {
            address_space.flush_on_all_cpus();
        }
        result
    }

    /// Creates a region for another address space. A `CopyOnWrite` region
    /// shares every backing frame with the new region. Every resident
    /// writable page of this region becomes read-only on every CPU before
    /// this call returns. Until then, other tasks of this process can write
    /// through a stale TLB entry. The new region sees those writes. No task
    /// uses the new region before this call returns. The fault handler of
    /// each side maps the pages that are not resident.
    ///
    /// A `CopyEager` region gets private copies of every resident page and
    /// no page of this region changes.
    ///
    /// # Errors
    /// Returns `SegmentReserved` when `target_vmm` already holds the region's
    /// address range. Returns `RemapFailed` when a resident page cannot
    /// become read-only. The pages that became read-only before the failure
    /// stay read-only. The next write fault restores each of them. Returns
    /// `OutOfMemory` or `MapFailed` when a `CopyEager` page cannot be copied.
    ///
    /// # Panics
    /// Panics if a resident page becomes read-only while interrupts are
    /// disabled.
    fn clone_into(
        &self,
        address_space: &AddressSpace,
        target_vmm: &impl VirtualMemoryAllocator,
    ) -> Result<Self, CloneRegionError> {
        let segment = target_vmm
            .mark_as_reserved(*self._segment)
            .map_err(|AlreadyReserved| CloneRegionError::SegmentReserved)?;
        if self.fork == ForkPolicy::CopyEager {
            return self.copy_into(address_space, segment);
        }

        let mut demoted = false;
        let state = {
            let state = self.state.lock();
            if state.flags.contains(PageTableFlags::WRITABLE) {
                let demote = |page: Page<Size4KiB>| match address_space
                    .remap::<Size4KiB, _>(page, |flags| flags - PageTableFlags::WRITABLE)
                {
                    Ok(()) => Ok(true),
                    Err(FlagUpdateError::PageNotMapped) => Ok(false),
                    Err(_) => Err(CloneRegionError::RemapFailed),
                };
                for (first, backing) in &state.ranges {
                    for i in 0..backing.len() {
                        demoted |= demote(*first + i)?;
                    }
                }
                for page in state.pages.keys() {
                    demoted |= demote(*page)?;
                }
            }
            PrivateRegionState {
                flags: state.flags,
                pages: state.pages.clone(),
                ranges: state.ranges.clone(),
            }
        };
        // See `AddressSpace::flush_on_all_cpus` for the lock rule.
        if demoted {
            address_space.flush_on_all_cpus();
        }

        Ok(Self {
            _segment: segment,
            start: self.start,
            size: self.size,
            fork: self.fork,
            per_thread: self.per_thread,
            state: Mutex::new(state),
        })
    }

    /// Copies every resident page into a private frame of the new region.
    /// The new region has no mapping until its address space pages it in.
    /// On failure the frames copied so far drop with the partial map.
    fn copy_into(
        &self,
        address_space: &AddressSpace,
        segment: OwnedSegment<'static>,
    ) -> Result<Self, CloneRegionError> {
        let state = self.state.lock();
        let range_pages = state.ranges.iter().flat_map(|(first, backing)| {
            (0..backing.len())
                .map(|i| (*first + i, backing.start + i))
                .filter(|(page, _)| !state.pages.contains_key(page))
        });
        let single_pages = state
            .pages
            .iter()
            .map(|(page, backing)| (*page, backing.start));

        let mut pages = BTreeMap::new();
        for (page, source) in range_pages.chain(single_pages) {
            let frame = PhysicalMemory::allocate_frame::<Size4KiB>()
                .ok_or(CloneRegionError::OutOfMemory)?;
            let owned = OwnedPhysicalMemory::from_physical_frame(frame);
            // `with_frame_mapped` takes the kernel half lock, so the two
            // mappings run one after the other.
            let bytes = with_frame_mapped(address_space, source, |src| *src)
                .map_err(|_| CloneRegionError::MapFailed)?;
            with_frame_mapped(address_space, frame, |dst| *dst = bytes)
                .map_err(|_| CloneRegionError::MapFailed)?;
            pages.insert(page, Arc::new(owned));
        }

        Ok(Self {
            _segment: segment,
            start: self.start,
            size: self.size,
            fork: self.fork,
            per_thread: self.per_thread,
            state: Mutex::new(PrivateRegionState {
                flags: state.flags,
                pages,
                ranges: BTreeMap::new(),
            }),
        })
    }

    pub fn start(&self) -> VirtAddr {
        self.start
    }

    /// # Errors
    /// This function returns `EagerBacking` when the region holds a
    /// contiguous kernel allocation, which is never split.
    fn check_split(&self) -> Result<(), SplitError> {
        if self.state.lock().ranges.is_empty() {
            Ok(())
        } else {
            Err(SplitError::EagerBacking)
        }
    }

    /// This function splits the region at `at`. The reservation, the
    /// accessible size and the pages divide at `at`. Both parts keep the
    /// flags, the fork policy and the per-thread mark.
    ///
    /// # Panics
    /// This function panics when [`PrivateMemoryRegion::check_split`] fails.
    /// It also panics when `at` is not a page boundary strictly inside the
    /// region.
    fn split(self, at: VirtAddr) -> (Self, Self) {
        let low_size = (at - self.start).into_usize();
        assert!(
            at.is_aligned(Size4KiB::SIZE) && self.start < at && low_size < self.size,
            "split point {at:p} is not a page inside the region"
        );

        let Self {
            _segment: segment,
            start,
            size,
            fork,
            per_thread,
            state,
        } = self;
        let PrivateRegionState {
            flags,
            pages: mut low_pages,
            ranges,
        } = state.into_inner();
        assert!(ranges.is_empty(), "an eager allocation is never split");
        let high_pages = low_pages.split_off(&Page::containing_address(at));
        let (low_segment, high_segment) = segment.split(at);
        let part = |segment, start, size, pages| Self {
            _segment: segment,
            start,
            size,
            fork,
            per_thread,
            state: Mutex::new(PrivateRegionState {
                flags,
                pages,
                ranges: BTreeMap::new(),
            }),
        };
        (
            part(low_segment, start, low_size, low_pages),
            part(high_segment, at, size - low_size, high_pages),
        )
    }

    fn unmap_from(&self, address_space: &AddressSpace) {
        let state = self.state.lock();
        for (first, backing) in &state.ranges {
            for i in 0..backing.len() {
                address_space.unmap::<Size4KiB>(*first + i);
            }
        }
        for page in state.pages.keys() {
            address_space.unmap::<Size4KiB>(*page);
        }
    }

    /// `fill` provides the content of a page that has no backing yet. It
    /// runs under the region lock, which cannot deadlock against mount
    /// locks because faulting while a mount lock is held is forbidden (see
    /// `make_user_range_resident`).
    fn handle_fault(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        caused_by_write: bool,
        fill: impl FnOnce(&mut [u8; 4096]) -> Result<(), PageInError>,
    ) -> Result<(), PageInError> {
        let mut state = self.state.lock();
        let flags = state.flags;
        if !flags.contains(PageTableFlags::USER_ACCESSIBLE) {
            return Err(PageInError::NotAccessible);
        }
        match address_space.translate_flags(page.start_address()) {
            // Mapped already, so a racing task resolved the fault or a
            // stale TLB entry produced it. Retrying makes progress. #PF
            // delivery invalidates the TLB entries for the faulting address.
            Some(f) if !caused_by_write || f.contains(PageTableFlags::WRITABLE) => Ok(()),
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
                    // See `AddressSpace::flush_on_all_cpus` for the lock rule.
                    drop(state);
                    // Other CPUs that run this process can still translate
                    // `page` to the shared frame.
                    address_space.flush_on_all_cpus();
                }
                Ok(())
            }
            None => match state.backing(page) {
                Some((count, frame)) => {
                    if !flags.contains(PageTableFlags::WRITABLE) {
                        if caused_by_write {
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
                    } else if !caused_by_write {
                        address_space
                            .map(page, frame, flags - PageTableFlags::WRITABLE)
                            .map_err(|_| PageInError::MapFailed)?;
                    } else {
                        // The copy completes before the page becomes visible.
                        // Therefore no other thread reads a half-copied page.
                        let new = PhysicalMemory::allocate_frame::<Size4KiB>()
                            .ok_or(PageInError::OutOfMemory)?;
                        let owned = OwnedPhysicalMemory::from_physical_frame(new);
                        with_frame_mapped(address_space, new, |dst| {
                            with_frame_mapped(address_space, frame, |src| {
                                dst.copy_from_slice(src);
                            })
                        })??;
                        address_space
                            .map(page, new, flags)
                            .map_err(|_| PageInError::MapFailed)?;
                        state.pages.insert(page, Arc::new(owned));
                    }
                    Ok(())
                }
                None => state.map_and_fill(address_space, page, fill),
            },
        }
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

    pub fn start(&self) -> VirtAddr {
        self.region.start()
    }

    /// This function splits the region at `at`, as
    /// [`PrivateMemoryRegion::split`] does. Each part reads the bytes of the
    /// file that back its own addresses.
    ///
    /// # Panics
    /// This function panics in each case in which
    /// [`PrivateMemoryRegion::split`] panics.
    fn split(self, at: VirtAddr) -> (Self, Self) {
        let Self {
            region,
            node,
            file_offset,
            file_len,
        } = self;
        let low_size = (at - region.start()).into_usize();
        let (low, high) = region.split(at);
        (
            Self {
                region: low,
                node: node.clone(),
                file_offset,
                file_len: file_len.min(low_size),
            },
            Self {
                region: high,
                node,
                file_offset: file_offset + low_size,
                file_len: file_len.saturating_sub(low_size),
            },
        )
    }

    fn handle_fault(
        &self,
        address_space: &AddressSpace,
        page: Page<Size4KiB>,
        caused_by_write: bool,
    ) -> Result<(), PageInError> {
        let off = (page.start_address() - self.region.start()).into_usize();
        let from_file = self
            .file_len
            .saturating_sub(off)
            .min(Size4KiB::SIZE.into_usize());
        self.region
            .handle_fault(address_space, page, caused_by_write, |buf| {
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

/// Owns the virtual reservation for a shared mapping and a share of the
/// backing memory.
///
/// It deliberately holds no [`OwnedPhysicalMemory`]. The device or the
/// `ShmFs` object owns those frames; `memory` keeps them alive while this
/// region exists. A drop of this region releases only the virtual range and
/// the share, never the frames.
#[derive(Debug)]
pub struct SharedMemoryRegion {
    segment: OwnedSegment<'static>,
    size: usize,
    /// Borrowed from `memory`. Never deallocated (see the struct docs).
    frames: PhysFrameRangeInclusive<Size4KiB>,
    memory: MmapRegion,
}

impl SharedMemoryRegion {
    pub fn new(
        segment: OwnedSegment<'static>,
        size: usize,
        frames: PhysFrameRangeInclusive<Size4KiB>,
        memory: MmapRegion,
    ) -> Self {
        Self {
            segment,
            size,
            frames,
            memory,
        }
    }

    pub fn start(&self) -> VirtAddr {
        self.segment.start
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
