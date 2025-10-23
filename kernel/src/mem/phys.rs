use alloc::vec::Vec;
use core::iter::from_fn;
use core::mem::swap;

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

/// A global interface to the kernel's physical memory allocator.
///
/// `PhysicalMemory` provides a zero-sized, stateless facade for managing physical memory frames
/// in the kernel. It wraps a global, lock-protected allocator that operates in two stages:
/// - **Stage 1**: A simple bump allocator used during early boot before the heap is initialized.
/// - **Stage 2**: A sophisticated bitmap allocator that efficiently manages physical memory
///   using a sparse representation to track only usable memory regions.
///
/// # Memory Management
///
/// Physical memory is managed in units called frames, which correspond to hardware page sizes.
/// The allocator supports three frame sizes:
/// - 4 KiB ([`Size4KiB`]): The standard small page size
/// - 2 MiB ([`Size2MiB`]): Huge pages for improved TLB performance
/// - 1 GiB ([`Size1GiB`]): Giant pages for large memory regions
///
/// [`Size4KiB`]: x86_64::structures::paging::Size4KiB
/// [`Size2MiB`]: x86_64::structures::paging::Size2MiB
/// [`Size1GiB`]: x86_64::structures::paging::Size1GiB
///
/// # Usage Patterns
///
/// ## Allocating Single Frames
///
/// ```no_run
/// use kernel::mem::phys::PhysicalMemory;
/// use x86_64::structures::paging::Size4KiB;
///
/// // Allocate a single 4 KiB frame
/// if let Some(frame) = PhysicalMemory::allocate_frame::<Size4KiB>() {
///     // Use the frame...
///     PhysicalMemory::deallocate_frame(frame);
/// }
/// ```
///
/// ## Allocating Contiguous Frame Ranges
///
/// ```no_run
/// use kernel::mem::phys::PhysicalMemory;
/// use x86_64::structures::paging::Size4KiB;
///
/// // Allocate 10 contiguous 4 KiB frames
/// if let Some(range) = PhysicalMemory::allocate_frames::<Size4KiB>(10) {
///     // Use the frames...
///     PhysicalMemory::deallocate_frames(range);
/// }
/// ```
///
/// ## Allocating Non-Contiguous Frames
///
/// ```no_run
/// use kernel::mem::phys::PhysicalMemory;
/// use x86_64::structures::paging::Size4KiB;
///
/// // Create an iterator that yields individual frames as needed
/// let frames = PhysicalMemory::allocate_frames_non_contiguous::<Size4KiB>();
/// for frame in frames.take(5) {
///     // Use each frame...
/// }
/// ```
///
/// # Thread Safety
///
/// All methods are thread-safe. The underlying allocator is protected by a spinlock,
/// ensuring safe concurrent access from multiple CPU cores.
///
/// # Initialization
///
/// The physical memory allocator must be initialized during boot before use.
/// Check initialization status with [`is_initialized()`](Self::is_initialized).
#[derive(Copy, Clone)]
pub struct PhysicalMemory;

#[allow(dead_code)]
impl PhysicalMemory {
    /// Checks whether the physical memory allocator has been initialized.
    ///
    /// The allocator is initialized in two stages during boot:
    /// 1. Stage 1 is initialized early, before the heap is available
    /// 2. Stage 2 is initialized later, after the heap is set up
    ///
    /// # Returns
    ///
    /// `true` if at least stage 1 initialization has completed, `false` otherwise.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use kernel::mem::phys::PhysicalMemory;
    ///
    /// if PhysicalMemory::is_initialized() {
    ///     // Safe to allocate frames
    /// } else {
    ///     // Allocator not yet ready
    /// }
    /// ```
    pub fn is_initialized() -> bool {
        PHYS_ALLOC.is_initialized()
    }

    /// Returns an iterator that yields non-contiguous physical frames on demand.
    ///
    /// Unlike [`allocate_frames()`](Self::allocate_frames), which requires contiguous frames,
    /// this method returns an iterator that allocates individual frames as needed. This is
    /// useful when contiguous physical memory is not required, allowing better memory
    /// utilization, especially when physical memory is fragmented.
    ///
    /// # Type Parameters
    ///
    /// * `S` - The page size for the frames. Must be one of [`Size4KiB`], [`Size2MiB`], or [`Size1GiB`].
    ///
    /// [`Size4KiB`]: x86_64::structures::paging::Size4KiB
    /// [`Size2MiB`]: x86_64::structures::paging::Size2MiB
    /// [`Size1GiB`]: x86_64::structures::paging::Size1GiB
    ///
    /// # Returns
    ///
    /// An iterator that yields [`PhysFrame<S>`] instances.
    /// The iterator terminates when physical memory is exhausted.
    ///
    /// [`PhysFrame<S>`]: x86_64::structures::paging::PhysFrame
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use kernel::mem::phys::PhysicalMemory;
    /// use x86_64::structures::paging::Size4KiB;
    ///
    /// // Allocate up to 100 frames (may be fewer if memory is exhausted)
    /// let frames: Vec<_> = PhysicalMemory::allocate_frames_non_contiguous::<Size4KiB>()
    ///     .take(100)
    ///     .collect();
    /// ```
    pub fn allocate_frames_non_contiguous<S: PageSize>() -> impl Iterator<Item = PhysFrame<S>>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        from_fn(Self::allocate_frame)
    }

    /// Allocates a single physical frame.
    ///
    /// This is the primary method for allocating physical memory. It attempts to allocate
    /// one frame of the specified page size. The frame is guaranteed to be properly aligned
    /// for the requested page size (e.g., 2 MiB alignment for 2 MiB pages).
    ///
    /// # Type Parameters
    ///
    /// * `S` - The page size for the frame. Must be one of [`Size4KiB`], [`Size2MiB`], or [`Size1GiB`].
    ///
    /// [`Size4KiB`]: x86_64::structures::paging::Size4KiB
    /// [`Size2MiB`]: x86_64::structures::paging::Size2MiB
    /// [`Size1GiB`]: x86_64::structures::paging::Size1GiB
    ///
    /// # Returns
    ///
    /// * `Some(frame)` - A properly aligned physical frame of the requested size
    /// * `None` - If no suitable frame is available (out of memory)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use kernel::mem::phys::PhysicalMemory;
    /// use x86_64::structures::paging::{Size4KiB, Size2MiB};
    ///
    /// // Allocate a 4 KiB frame
    /// let small_frame = PhysicalMemory::allocate_frame::<Size4KiB>()
    ///     .expect("out of memory");
    ///
    /// // Allocate a 2 MiB huge page
    /// let huge_frame = PhysicalMemory::allocate_frame::<Size2MiB>()
    ///     .expect("out of memory");
    /// ```
    ///
    /// # Notes
    ///
    /// - Frames allocated with this method should be deallocated with [`deallocate_frame()`](Self::deallocate_frame)
    /// - This method is lock-protected and safe to call from any context
    /// - A warning is logged when allocation fails due to memory exhaustion
    #[must_use]
    pub fn allocate_frame<S: PageSize>() -> Option<PhysFrame<S>>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().allocate_frame()
    }

    /// Allocates multiple contiguous physical frames.
    ///
    /// Attempts to allocate `n` contiguous frames of the specified size. This is more efficient
    /// than allocating frames individually and is required for operations that need physically
    /// contiguous memory, such as DMA transfers or mapping large memory regions.
    ///
    /// The allocator searches for a contiguous range of free frames that meet the alignment
    /// requirements for the requested page size. For example, 2 MiB frames must start at
    /// a 2 MiB-aligned address.
    ///
    /// # Type Parameters
    ///
    /// * `S` - The page size for the frames. Must be one of [`Size4KiB`], [`Size2MiB`], or [`Size1GiB`].
    ///
    /// [`Size4KiB`]: x86_64::structures::paging::Size4KiB
    /// [`Size2MiB`]: x86_64::structures::paging::Size2MiB
    /// [`Size1GiB`]: x86_64::structures::paging::Size1GiB
    ///
    /// # Arguments
    ///
    /// * `n` - The number of contiguous frames to allocate
    ///
    /// # Returns
    ///
    /// * `Some(range)` - An inclusive range of contiguous, properly aligned frames
    /// * `None` - If no contiguous region of sufficient size is available
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use kernel::mem::phys::PhysicalMemory;
    /// use x86_64::structures::paging::Size4KiB;
    ///
    /// // Allocate 256 contiguous 4 KiB frames (1 MiB total)
    /// if let Some(range) = PhysicalMemory::allocate_frames::<Size4KiB>(256) {
    ///     // The frames from range.start to range.end are contiguous in physical memory
    ///     for frame in range {
    ///         // Process each frame...
    ///     }
    /// }
    /// ```
    ///
    /// # Notes
    ///
    /// - Ranges allocated with this method should be deallocated with [`deallocate_frames()`](Self::deallocate_frames)
    /// - This method may fail even when sufficient total memory exists if it's fragmented
    /// - Stage 1 allocator does not support this method and will panic if called before stage 2
    #[must_use]
    pub fn allocate_frames<S: PageSize>(n: usize) -> Option<PhysFrameRangeInclusive<S>>
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().allocate_frames(n)
    }

    /// Deallocates a single physical frame, returning it to the free pool.
    ///
    /// This method marks the frame as free, making it available for future allocations.
    /// The frame must have been previously allocated with [`allocate_frame()`](Self::allocate_frame).
    ///
    /// # Type Parameters
    ///
    /// * `S` - The page size of the frame being deallocated
    ///
    /// # Arguments
    ///
    /// * `frame` - The frame to deallocate
    ///
    /// # Panics
    ///
    /// When built with debug assertions, this method panics if:
    /// - The frame is already free
    /// - The frame was never allocated
    /// - Stage 1 allocator is active (deallocation not supported in stage 1)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use kernel::mem::phys::PhysicalMemory;
    /// use x86_64::structures::paging::Size4KiB;
    ///
    /// let frame = PhysicalMemory::allocate_frame::<Size4KiB>()
    ///     .expect("out of memory");
    ///
    /// // Use the frame...
    ///
    /// PhysicalMemory::deallocate_frame(frame);
    /// // The frame is now available for reallocation
    /// ```
    ///
    /// # Notes
    ///
    /// - For 2 MiB and 1 GiB frames, this method deallocates all constituent 4 KiB frames
    /// - Double-freeing a frame is a programming error and will be caught in debug builds
    /// - This method is lock-protected and safe to call from any context
    pub fn deallocate_frame<S: PageSize>(frame: PhysFrame<S>)
    where
        PhysicalMemoryManager: PhysicalFrameAllocator<S>,
    {
        allocator().lock().deallocate_frame(frame);
    }

    /// Deallocates a range of contiguous physical frames.
    ///
    /// This is the counterpart to [`allocate_frames()`](Self::allocate_frames). It deallocates
    /// all frames in the given range, returning them to the free pool.
    ///
    /// # Type Parameters
    ///
    /// * `S` - The page size of the frames being deallocated
    ///
    /// # Arguments
    ///
    /// * `range` - An inclusive range of frames to deallocate
    ///
    /// # Panics
    ///
    /// When built with debug assertions, this method panics if:
    /// - Any frame in the range is already free
    /// - Any frame in the range was never allocated
    /// - Stage 1 allocator is active (deallocation not supported in stage 1)
    ///
    /// If a panic occurs, deallocation stops and remaining frames are not freed.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use kernel::mem::phys::PhysicalMemory;
    /// use x86_64::structures::paging::Size4KiB;
    ///
    /// let range = PhysicalMemory::allocate_frames::<Size4KiB>(100)
    ///     .expect("out of memory");
    ///
    /// // Use the frames...
    ///
    /// PhysicalMemory::deallocate_frames(range);
    /// // All frames in the range are now available for reallocation
    /// ```
    ///
    /// # Notes
    ///
    /// - This method iterates through the range and deallocates each frame individually
    /// - Double-freeing any frame in the range is a programming error
    /// - This method is lock-protected and safe to call from any context
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
