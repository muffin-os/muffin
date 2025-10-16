use alloc::vec;
use alloc::vec::Vec;
use core::iter::from_fn;
use core::mem::swap;

use conquer_once::spin::OnceCell;
use kernel_physical_memory::{FrameState, PhysicalFrameAllocator, PhysicalMemoryManager};
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

#[derive(Copy, Clone)]
pub struct PhysicalMemory;

#[allow(dead_code)]
impl PhysicalMemory {
    pub fn is_initialized() -> bool {
        PHYS_ALLOC.is_initialized()
    }

    pub fn allocate_frames_non_contiguous<S: PageSize>() -> impl Iterator<Item = PhysFrame<S>>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        from_fn(Self::allocate_frame)
    }

    /// Calls [`FrameAllocator::allocate_frame`] on the current physical allocator.
    #[must_use]
    pub fn allocate_frame<S: PageSize>() -> Option<PhysFrame<S>>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().allocate_frame()
    }

    /// Calls [`FrameAllocator::allocate_frames`] on the current physical allocator.
    #[must_use]
    pub fn allocate_frames<S: PageSize>(n: usize) -> Option<PhysFrameRangeInclusive<S>>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().allocate_frames(n)
    }

    /// Calls [`FrameAllocator::deallocate_frame`] on the current physical allocator.
    pub fn deallocate_frame<S: PageSize>(frame: PhysFrame<S>)
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().deallocate_frame(frame);
    }

    /// Calls [`FrameAllocator::deallocate_frames`] on the current physical allocator.
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
pub(in crate::mem) fn init_stage1(entries: &'static [&'static Entry]) {
    let usable_physical_memory = entries
        .iter()
        .filter(|e| e.entry_type == EntryType::USABLE)
        .map(|e| e.length)
        .sum::<u64>();
    info!("usable RAM: ~{} MiB", usable_physical_memory / 1024 / 1024);

    let stage1 = MultiStageAllocator::Stage1(PhysicalBumpAllocator::new(entries));
    PHYS_ALLOC.init_once(|| Mutex::new(stage1));
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
    let mut memory_regions = Vec::new();
    for entry in regions.iter().filter(|r| r.entry_type == EntryType::USABLE) {
        let num_frames = (entry.length / Size4KiB::SIZE) as usize;
        let region = kernel_physical_memory::MemoryRegion::new(
            entry.base,
            num_frames,
            kernel_physical_memory::FrameState::Free,
        );
        memory_regions.push(region);
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
