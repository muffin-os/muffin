use alloc::vec::Vec;
use core::iter::from_fn;
use core::mem::{ManuallyDrop, swap};
use core::ops::Deref;

use conquer_once::spin::OnceCell;
use kernel_physical_memory::{PhysicalFrameAllocator, PhysicalMemoryManager};
use limine::memory_map::{Entry, EntryType};
use log::{info, warn};
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PhysFrame, Size4KiB};

use crate::mem::heap::Heap;

static PHYS_ALLOC: OnceCell<Mutex<MultiStageAllocator>> = OnceCell::uninit();

fn allocator() -> &'static Mutex<MultiStageAllocator> {
    PHYS_ALLOC
        .get()
        .expect("physical allocator not initialized")
}

#[derive(Debug, Eq, PartialEq)]
pub struct OwnedPhysicalMemory {
    range: PhysFrameRangeInclusive<Size4KiB>,
}

impl !Clone for OwnedPhysicalMemory {}

impl OwnedPhysicalMemory {
    pub fn from_physical_frame<S: PageSize>(frame: PhysFrame<S>) -> Self {
        let start_address = frame.start_address();
        let start = unsafe {
            // Safety: safe because the start address is guaranteed to be valid
            PhysFrame::<Size4KiB>::from_start_address_unchecked(start_address)
        };
        let end = PhysFrame::<Size4KiB>::containing_address(start_address + S::SIZE - 1);
        Self {
            range: PhysFrameRangeInclusive { start, end },
        }
    }

    pub fn from_physical_frame_range<S: PageSize>(range: PhysFrameRangeInclusive<S>) -> Self {
        let start_address = range.start.start_address();
        let start = PhysFrame::<Size4KiB>::containing_address(start_address);
        let end_address = range.end.start_address() + range.end.size() - 1;
        let end = PhysFrame::<Size4KiB>::containing_address(end_address);
        Self {
            range: PhysFrameRangeInclusive { start, end },
        }
    }

    pub fn is_single_frame(&self) -> bool {
        self.range.start == self.range.end
    }

    pub fn leak(self) -> PhysFrameRangeInclusive<Size4KiB> {
        ManuallyDrop::new(self).range
    }
}

impl Deref for OwnedPhysicalMemory {
    type Target = PhysFrameRangeInclusive<Size4KiB>;

    fn deref(&self) -> &Self::Target {
        &self.range
    }
}

impl Drop for OwnedPhysicalMemory {
    fn drop(&mut self) {
        PhysicalMemory::deallocate_frames(self.range);
    }
}

/// Zero-sized facade to the global physical memory allocator.
///
/// This provides a stateless interface to a two-stage allocator:
/// - **Stage 1**: A bump allocator used during early boot before the heap is initialized.
///   Only supports 4 KiB allocations, no deallocation.
/// - **Stage 2**: A sparse bitmap allocator that efficiently tracks free frames across
///   non-contiguous memory regions. Supports all page sizes and deallocation.
#[derive(Copy, Clone)]
pub struct PhysicalMemory;

#[allow(dead_code)]
impl PhysicalMemory {
    /// Checks whether the physical memory allocator has been initialized.
    ///
    /// Returns `true` after stage 1 initialization completes during early boot.
    pub fn is_initialized() -> bool {
        PHYS_ALLOC.is_initialized()
    }

    /// Returns an iterator that allocates individual frames on demand.
    ///
    /// Unlike [`allocate_frames()`](Self::allocate_frames), this doesn't require finding
    /// contiguous physical memory. Useful when physical memory is fragmented or when the
    /// caller doesn't need physically contiguous memory (e.g., for page tables that use
    /// virtual addressing anyway).
    ///
    /// Each call to `next()` acquires and releases the allocator's spinlock, so batch
    /// allocation with [`allocate_frames()`](Self::allocate_frames) is more efficient when
    /// contiguous memory is available. Do not call with interrupts disabled.
    pub fn allocate_frames_non_contiguous<S: PageSize>() -> impl Iterator<Item = PhysFrame<S>>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        from_fn(Self::allocate_frame)
    }

    /// Allocates a single physical frame of the specified page size.
    ///
    /// Returns a properly aligned frame (4 KiB aligned for 4 KiB pages, 2 MiB aligned for
    /// 2 MiB pages, etc.). For large page sizes (2 MiB, 1 GiB), this searches for a
    /// sufficiently large aligned region, which may fail even if enough total memory exists
    /// but is fragmented.
    ///
    /// Acquires the allocator's spinlock, so do not call with interrupts disabled.
    #[must_use]
    pub fn allocate_frame<S: PageSize>() -> Option<PhysFrame<S>>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().allocate_frame()
    }

    /// Allocates `n` contiguous frames of the specified page size.
    ///
    /// Searches for a contiguous, properly aligned region of physical memory. This is
    /// required for DMA operations and more efficient than using the non-contiguous
    /// iterator for bulk allocations. May fail even when sufficient total memory exists
    /// if the memory is fragmented.
    ///
    /// Acquires the allocator's spinlock, so do not call with interrupts disabled.
    ///
    /// # Panics
    ///
    /// Panics if stage 1 allocator is still active (stage 1 doesn't support contiguous
    /// allocation). Stage 2 is initialized after the heap becomes available.
    #[must_use]
    pub fn allocate_frames<S: PageSize>(n: usize) -> Option<OwnedPhysicalMemory>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator()
            .lock()
            .allocate_frames(n)
            .map(OwnedPhysicalMemory::from_physical_frame_range)
    }

    /// Returns the frame to the free pool for future allocations.
    ///
    /// For large pages (2 MiB, 1 GiB), this deallocates all constituent 4 KiB frames.
    /// Double-freeing a frame is a bug and will panic in debug builds.
    ///
    /// Acquires the allocator's spinlock, so do not call with interrupts disabled.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if:
    /// - The frame is already free or was never allocated
    /// - Stage 1 allocator is active (stage 1 doesn't support deallocation)
    pub fn deallocate_frame<S: PageSize>(frame: PhysFrame<S>)
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().deallocate_frame(frame);
    }

    /// Deallocates all frames in the range, returning them to the free pool.
    ///
    /// Iterates through the range deallocating each frame individually. If a panic occurs
    /// during iteration (e.g., double-free detected in debug build), remaining frames are
    /// not deallocated.
    ///
    /// Acquires the allocator's spinlock, so do not call with interrupts disabled.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if:
    /// - Any frame in the range is already free or was never allocated
    /// - Stage 1 allocator is active (stage 1 doesn't support deallocation)
    pub fn deallocate_frames<S: PageSize>(range: PhysFrameRangeInclusive<S>)
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().deallocate_frames(range);
    }
}

unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for PhysicalMemory {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        Self::allocate_frame()
    }
}

/// Initialize the first stage of physical memory management: a simple bump
/// allocator.
///
/// Returns the total amount of usable physical memory in bytes.
pub(in crate::mem) fn init_stage1(entries: &'static [&'static Entry]) -> usize {
    let usable_physical_memory = entries
        .iter()
        .filter(|e| e.entry_type == EntryType::USABLE)
        .map(|e| e.length)
        .sum::<u64>();
    info!("usable RAM: ~{} MiB", usable_physical_memory / 1024 / 1024);

    let stage1 = MultiStageAllocator::Stage1(PhysicalBumpAllocator::new(entries));
    PHYS_ALLOC.init_once(|| Mutex::new(stage1));

    usable_physical_memory as usize
}

/// Initialize the second stage of physical memory management: a bitmap allocator.
/// This allocator requires that the heap is initialized and that stage1 was previously
/// initialized.
pub(in crate::mem) fn init_stage2() {
    let mut guard = allocator().lock();

    let MultiStageAllocator::Stage1(stage1) = &*guard else {
        unreachable!()
    };

    assert!(Heap::is_initialized());

    let regions = stage1.regions;
    let stage_one_next_free = stage1.next_frame;

    /*
    Limine guarantees that
    1. USABLE regions do not overlap
    2. USABLE regions are sorted by base address, lowest to highest
    3. USABLE regions are 4KiB aligned (address and length)
     */

    // Build memory regions for usable regions
    // Preallocate to avoid fragmentation in stage1 (which can't deallocate)
    let usable_region_count = regions
        .iter()
        .filter(|r| r.entry_type == EntryType::USABLE)
        .count();
    let mut memory_regions = Vec::with_capacity(usable_region_count);

    for entry in regions.iter().filter(|r| r.entry_type == EntryType::USABLE) {
        let num_frames = (entry.length / Size4KiB::SIZE) as usize;
        let region = kernel_physical_memory::MemoryRegion::new(
            entry.base,
            num_frames,
            kernel_physical_memory::FrameState::Free,
        );
        memory_regions
            .push_within_capacity(region)
            .expect("preallocated capacity should be sufficient");
    }

    // Mark frames allocated by stage1
    for frame in stage1.usable_frames().take(stage_one_next_free) {
        let addr = frame.start_address().as_u64();
        // Find which region this frame belongs to and mark it as allocated
        for region in &mut memory_regions {
            if let Some(idx) = region.frame_index(addr) {
                region.frames_mut()[idx] = kernel_physical_memory::FrameState::Allocated;
                break;
            }
        }
    }

    // Create sparse physical memory manager - much more memory efficient!
    let bitmap_allocator = PhysicalMemoryManager::new(memory_regions);
    let mut stage2 = MultiStageAllocator::Stage2(bitmap_allocator);
    swap(&mut *guard, &mut stage2);
}

pub trait FrameAllocator<S: PageSize> {
    /// Allocates a single physical frame. If there is no more physical memory,
    /// this function returns `None`.
    fn allocate_frame(&mut self) -> Option<PhysFrame<S>> {
        self.allocate_frames(1).map(|range| range.start)
    }

    /// Allocates `n` contiguous physical frames. If there is no more physical
    /// memory, this function returns `None`.
    fn allocate_frames(&mut self, n: usize) -> Option<PhysFrameRangeInclusive<S>>;

    /// Deallocates a single physical frame.
    ///
    /// # Panics
    /// If built with `debug_assertions`, this function panics if the frame is
    /// already deallocated or not allocated yet.
    fn deallocate_frame(&mut self, frame: PhysFrame<S>);

    /// Deallocates a range of physical frames.
    ///
    /// # Panics
    /// If built with `debug_assertions`, this function panics if any frame in
    /// the range is already deallocated or not allocated yet.
    /// Deallocation of remaining frames will not be attempted.
    fn deallocate_frames(&mut self, range: PhysFrameRangeInclusive<S>) {
        for frame in range {
            self.deallocate_frame(frame);
        }
    }
}

enum MultiStageAllocator {
    Stage1(PhysicalBumpAllocator),
    Stage2(PhysicalMemoryManager),
}

impl<S: PageSize> FrameAllocator<S> for MultiStageAllocator
where
    PhysicalMemoryManager: PhysicalFrameAllocator<S>,
{
    fn allocate_frame(&mut self) -> Option<PhysFrame<S>> {
        let res = match self {
            Self::Stage1(a) => {
                if S::SIZE == Size4KiB::SIZE {
                    Some(
                        PhysFrame::<S>::from_start_address(a.allocate_frame()?.start_address())
                            .unwrap(),
                    )
                } else {
                    unimplemented!("can't allocate non-4KiB frames in stage1")
                }
            }
            Self::Stage2(a) => a.allocate_frame(),
        };
        if res.is_none() {
            warn!("out of physical memory");
        }
        res
    }

    fn allocate_frames(&mut self, n: usize) -> Option<PhysFrameRangeInclusive<S>> {
        match self {
            Self::Stage1(_) => unimplemented!("can't allocate contiguous frames in stage1"),
            Self::Stage2(a) => a.allocate_frames(n),
        }
    }

    fn deallocate_frame(&mut self, frame: PhysFrame<S>) {
        match self {
            Self::Stage1(_) => unimplemented!("can't deallocate frames in stage1"),
            Self::Stage2(a) => {
                a.deallocate_frame(frame);
            }
        }
    }

    fn deallocate_frames(&mut self, range: PhysFrameRangeInclusive<S>) {
        match self {
            Self::Stage1(_) => unimplemented!("can't deallocate frames in stage1"),
            Self::Stage2(a) => {
                a.deallocate_frames(range);
            }
        }
    }
}

struct PhysicalBumpAllocator {
    regions: &'static [&'static Entry],
    next_frame: usize,
}

impl PhysicalBumpAllocator {
    fn new(regions: &'static [&'static Entry]) -> Self {
        Self {
            regions,
            next_frame: 0,
        }
    }

    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        self.regions
            .iter()
            .filter(|region| region.entry_type == EntryType::USABLE)
            .map(|region| region.base..region.length)
            .flat_map(|r| r.step_by(usize::try_from(Size4KiB::SIZE).expect("usize overflow")))
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }

    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next_frame);
        if frame.is_some() {
            self.next_frame += 1;
        }
        frame
    }
}
