#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use x86_64::PhysAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PhysFrame, Size1GiB, Size2MiB, Size4KiB};

mod region;
pub use region::MemoryRegion;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FrameState {
    Unusable,
    Allocated,
    Free,
}

impl FrameState {
    #[must_use]
    pub fn is_usable(self) -> bool {
        !matches!(self, Self::Unusable)
    }
}

/// A position in the sparse memory manager containing both the region index
/// and the frame index within that region. This ensures that region and index
/// are always consistent.
#[derive(Debug, Copy, Clone)]
struct RegionFrameIndex {
    region_idx: usize,
    frame_idx: usize,
}

/// A physical memory manager that keeps track of the state of each frame in the
/// system using a sparse representation that only tracks usable memory regions.
pub struct PhysicalMemoryManager {
    regions: Vec<MemoryRegion>,
    first_free: Option<RegionFrameIndex>,
}

impl PhysicalMemoryManager {
    /// Creates a new manager from usable memory regions.
    ///
    /// # Arguments
    /// * `regions` - Pre-allocated vector of memory regions. Each region should already have
    ///   frames marked as Free or Allocated based on stage1 allocations.
    #[must_use]
    pub fn new(regions: Vec<MemoryRegion>) -> Self {
        let first_free = Self::find_first_free_internal(&regions);
        Self {
            regions,
            first_free,
        }
    }

    /// Find the region and local index for a given physical address
    fn find_frame_location(regions: &[MemoryRegion], addr: u64) -> Option<RegionFrameIndex> {
        for (region_idx, region) in regions.iter().enumerate() {
            if let Some(frame_idx) = region.frame_index(addr) {
                return Some(RegionFrameIndex {
                    region_idx,
                    frame_idx,
                });
            }
        }
        None
    }

    /// Internal helper to find the first free frame across all regions
    fn find_first_free_internal(regions: &[MemoryRegion]) -> Option<RegionFrameIndex> {
        for (region_idx, region) in regions.iter().enumerate() {
            if let Some(frame_idx) = region.frames().iter().position(|&s| s == FrameState::Free) {
                return Some(RegionFrameIndex {
                    region_idx,
                    frame_idx,
                });
            }
        }
        None
    }

    /// Get the current first free frame position
    fn first_free(&self) -> Option<RegionFrameIndex> {
        self.first_free
    }

    /// Update the first_free pointer starting from a given position
    fn update_first_free(&mut self, start_region: usize, start_index: usize) {
        // Check if there are more free frames in the current region
        if let Some(region) = self.regions.get(start_region)
            && start_index < region.len()
            && let Some(idx) = region.frames()[start_index..]
                .iter()
                .position(|&s| s == FrameState::Free)
        {
            self.first_free = Some(RegionFrameIndex {
                region_idx: start_region,
                frame_idx: start_index + idx,
            });
            return;
        }

        // Search subsequent regions
        for (region_idx, region) in self.regions.iter().enumerate().skip(start_region + 1) {
            if let Some(frame_idx) = region.frames().iter().position(|&s| s == FrameState::Free) {
                self.first_free = Some(RegionFrameIndex {
                    region_idx,
                    frame_idx,
                });
                return;
            }
        }

        // No free frames found
        self.first_free = None;
    }

    fn allocate_frames_impl<S: PageSize>(
        &mut self,
        n: usize,
    ) -> Option<PhysFrameRangeInclusive<S>> {
        let small_frames_per_frame = (S::SIZE / Size4KiB::SIZE) as usize;
        let small_frame_count = n * small_frames_per_frame;

        let ff = self.first_free()?;

        // TODO: Support searching across region boundaries for better memory utilization
        // Search for contiguous free frames within regions
        for region_idx in ff.region_idx..self.regions.len() {
            let search_start = if region_idx == ff.region_idx {
                ff.frame_idx
            } else {
                0
            };

            let region = &self.regions[region_idx];
            if search_start >= region.len() {
                continue;
            }

            // Align search_start up to the required page size alignment
            let aligned_search_start = {
                let offset = search_start % small_frames_per_frame;
                if offset == 0 {
                    search_start
                } else {
                    search_start + (small_frames_per_frame - offset)
                }
            };

            // Search for contiguous free frames
            let mut current_start = aligned_search_start;
            while current_start + small_frame_count <= region.len() {
                // Check if we have enough contiguous free frames
                let all_free = region.frames()[current_start..current_start + small_frame_count]
                    .iter()
                    .all(|&state| state == FrameState::Free);

                if all_free {
                    let frame_start_idx = current_start;
                    let frame_end_idx = current_start + small_frame_count - 1;

                    // Get the physical addresses before mutating
                    let start_addr = self.regions[region_idx].frame_address(frame_start_idx)?;
                    let end_addr_idx =
                        frame_end_idx / small_frames_per_frame * small_frames_per_frame;
                    let end_addr = self.regions[region_idx].frame_address(end_addr_idx)?;

                    // Mark frames as allocated
                    self.regions[region_idx].frames_mut()[frame_start_idx..=frame_end_idx]
                        .fill(FrameState::Allocated);

                    // Update first_free pointers
                    if region_idx == ff.region_idx && frame_start_idx <= ff.frame_idx {
                        self.update_first_free(region_idx, frame_end_idx + 1);
                    }

                    // Convert to physical frames
                    return Some(PhysFrameRangeInclusive {
                        start: PhysFrame::from_start_address(PhysAddr::new(start_addr)).ok()?,
                        end: PhysFrame::from_start_address(PhysAddr::new(end_addr)).ok()?,
                    });
                }

                // Move to next aligned position
                current_start += small_frames_per_frame;
            }
        }

        None
    }

    /// Converts a 4KiB frame index to a physical frame, if that frame index
    /// aligns with the page size [`S`] and the index is within a usable region.
    ///
    /// For example, if [`S`] is [`Size4KiB`], the frame index must be a multiple
    /// of 1, if [`S`] is [`Size2MiB`], the frame index must be a multiple of 512
    /// and so on.
    ///
    /// Calling this function with an index of 2 (address 0x2000) and [`S`] being
    /// [`Size2MiB`] will return [`None`], since frame index 2 is not 2MiB aligned.
    fn index_to_frame<S: PageSize>(&self, index: usize) -> Option<PhysFrame<S>> {
        let addr = index as u64 * Size4KiB::SIZE;

        // address must be aligned to [`S`]'s page size
        if !addr.is_multiple_of(S::SIZE) {
            return None;
        }

        // Check if address is in a usable region
        for region in &self.regions {
            if region.frame_index(addr).is_some() {
                return Some(PhysFrame::containing_address(PhysAddr::new(addr)));
            }
        }

        None
    }

    fn frame_to_index<S: PageSize>(&self, frame: PhysFrame<S>) -> Option<usize> {
        let addr = frame.start_address().as_u64();

        // Check if frame is in a usable region
        for region in &self.regions {
            if region.frame_index(addr).is_some() {
                let index = (addr / Size4KiB::SIZE) as usize;
                return Some(index);
            }
        }

        None
    }
}

pub trait PhysicalFrameAllocator<S: PageSize> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<S>> {
        self.allocate_frames(1).map(|range| range.start)
    }

    fn allocate_frames(&mut self, n: usize) -> Option<PhysFrameRangeInclusive<S>>;

    fn deallocate_frame(&mut self, frame: PhysFrame<S>) -> Option<PhysFrame<S>>;

    fn deallocate_frames(
        &mut self,
        range: PhysFrameRangeInclusive<S>,
    ) -> Option<PhysFrameRangeInclusive<S>> {
        let mut res: Option<PhysFrameRangeInclusive<S>> = None;
        for frame in range {
            let frame = self.deallocate_frame(frame)?;
            let start = if let Some(r) = res { r.start } else { frame };
            res = Some(PhysFrameRangeInclusive { start, end: frame });
        }
        res
    }
}

impl PhysicalFrameAllocator<Size4KiB> for PhysicalMemoryManager {
    fn allocate_frames(&mut self, n: usize) -> Option<PhysFrameRangeInclusive<Size4KiB>> {
        self.allocate_frames_impl(n)
    }

    fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) -> Option<PhysFrame<Size4KiB>> {
        let addr = frame.start_address().as_u64();

        // Find which region contains this frame
        let loc = Self::find_frame_location(&self.regions, addr)?;

        if self.regions[loc.region_idx].frames()[loc.frame_idx] == FrameState::Allocated {
            self.regions[loc.region_idx].frames_mut()[loc.frame_idx] = FrameState::Free;

            // Update first_free if this is before the current first_free
            let is_before_first_free = match self.first_free {
                Some(ff) => {
                    loc.region_idx < ff.region_idx
                        || (loc.region_idx == ff.region_idx && loc.frame_idx < ff.frame_idx)
                }
                None => true,
            };

            if is_before_first_free {
                self.first_free = Some(loc);
            }

            Some(frame)
        } else {
            None
        }
    }
}

impl PhysicalFrameAllocator<Size2MiB> for PhysicalMemoryManager {
    fn allocate_frames(&mut self, n: usize) -> Option<PhysFrameRangeInclusive<Size2MiB>> {
        self.allocate_frames_impl(n)
    }

    fn deallocate_frame(&mut self, frame: PhysFrame<Size2MiB>) -> Option<PhysFrame<Size2MiB>> {
        for i in 0..(Size2MiB::SIZE / Size4KiB::SIZE) as usize {
            let frame = PhysFrame::<Size4KiB>::containing_address(
                frame.start_address() + (i as u64 * Size4KiB::SIZE),
            );
            self.deallocate_frame(frame)?;
        }

        Some(frame)
    }
}

impl PhysicalFrameAllocator<Size1GiB> for PhysicalMemoryManager {
    fn allocate_frames(&mut self, n: usize) -> Option<PhysFrameRangeInclusive<Size1GiB>> {
        self.allocate_frames_impl(n)
    }

    fn deallocate_frame(&mut self, frame: PhysFrame<Size1GiB>) -> Option<PhysFrame<Size1GiB>> {
        for i in 0..(Size1GiB::SIZE / Size2MiB::SIZE) as usize {
            let frame = PhysFrame::<Size2MiB>::containing_address(
                frame.start_address() + (i as u64 * Size2MiB::SIZE),
            );
            self.deallocate_frame(frame)?;
        }

        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn test_new() {
        let states = vec![
            FrameState::Free,
            FrameState::Allocated,
            FrameState::Unusable,
            FrameState::Free,
        ];
        let region = MemoryRegion::with_frames(0, states.clone());
        let pmm = PhysicalMemoryManager::new(vec![region]);
        assert_eq!(1, pmm.regions.len());
        assert_eq!(4, pmm.regions[0].len());
        assert_eq!(&states[..], pmm.regions[0].frames());
    }

    #[test]
    fn test_new_trailing_unusable() {
        let states = vec![FrameState::Unusable, FrameState::Free, FrameState::Unusable];
        let region = MemoryRegion::with_frames(0, states.clone());
        let pmm = PhysicalMemoryManager::new(vec![region]);
        assert_eq!(1, pmm.regions.len());
        assert_eq!(3, pmm.regions[0].len());
        assert_eq!(&states[..], pmm.regions[0].frames());
    }

    #[test]
    fn test_new_no_frames() {
        let pmm = PhysicalMemoryManager::new(vec![]);
        assert!(pmm.regions.is_empty());
    }

    #[test]
    fn test_allocate_deallocate_4kib() {
        let region = MemoryRegion::new(0, 4, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);
        assert_eq!(1, pmm.regions.len());
        assert_eq!(4, pmm.regions[0].len());
        let frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame4: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        assert_eq!(Option::<PhysFrame<Size4KiB>>::None, pmm.allocate_frame());

        assert_eq!(Some(frame2), pmm.deallocate_frame(frame2));
        assert_eq!(None, pmm.deallocate_frame(frame2));

        assert_eq!(Some(frame4), pmm.deallocate_frame(frame4));
        assert_eq!(Some(frame2), pmm.allocate_frame());

        assert_eq!(Some(frame1), pmm.deallocate_frame(frame1));
        assert_eq!(Some(frame3), pmm.deallocate_frame(frame3));

        assert_eq!(Some(frame2), pmm.deallocate_frame(frame2));
        assert_eq!(1, pmm.regions.len());
        assert_eq!(4, pmm.regions[0].len());
    }

    #[test]
    fn test_allocate_deallocate_2mib() {
        let region = MemoryRegion::new(0, 1024, FrameState::Free); // 4MiB
        let mut pmm = PhysicalMemoryManager::new(vec![region]);
        let small_frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap(); // force alignment

        let frame1: PhysFrame<Size2MiB> = pmm.allocate_frame().unwrap();
        assert_eq!(512 * 4096, frame1.start_address().as_u64());

        assert_eq!(Option::<PhysFrame<Size2MiB>>::None, pmm.allocate_frame());
        let small_frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(Some(small_frame1), pmm.deallocate_frame(small_frame1));
        assert_eq!(Some(frame1), pmm.deallocate_frame(frame1));
        assert_eq!(Some(small_frame2), pmm.deallocate_frame(small_frame2));
    }

    #[cfg(not(miri))] // this just takes too long
    #[test]
    fn test_allocate_deallocate_1gib() {
        let region = MemoryRegion::new(0, 512 * 512 * 2, FrameState::Free); // 2GiB
        let mut pmm = PhysicalMemoryManager::new(vec![region]);
        let small_frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap(); // force alignment

        let frame1: PhysFrame<Size1GiB> = pmm.allocate_frame().unwrap();
        assert_eq!(1024 * 1024 * 1024, frame1.start_address().as_u64());

        assert_eq!(Option::<PhysFrame<Size1GiB>>::None, pmm.allocate_frame());
        let small_frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(Some(small_frame1), pmm.deallocate_frame(small_frame1));
        assert_eq!(Some(frame1), pmm.deallocate_frame(frame1));
        assert_eq!(Some(small_frame2), pmm.deallocate_frame(small_frame2));
    }

    #[test]
    fn test_sparse_multiple_regions() {
        // Create manager with two separate regions
        let region1 = MemoryRegion::new(0x0000_0000, 4, FrameState::Free);
        let region2 = MemoryRegion::new(0x1000_0000, 4, FrameState::Free);
        let pmm = PhysicalMemoryManager::new(vec![region1, region2]);

        assert_eq!(2, pmm.regions.len());
        assert_eq!(4, pmm.regions[0].len());
        assert_eq!(4, pmm.regions[1].len());
        assert_eq!(0x0000_0000, pmm.regions[0].base_addr());
        assert_eq!(0x1000_0000, pmm.regions[1].base_addr());
    }

    #[test]
    fn test_sparse_allocate_deallocate() {
        // Create sparse manager with gaps between regions
        let region1 = MemoryRegion::new(0x0000_0000, 4, FrameState::Free);
        let region2 = MemoryRegion::new(0x1000_0000, 4, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region1, region2]);

        // Allocate from first region
        let frame1: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x0000, frame1.start_address().as_u64());

        let frame2: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x1000, frame2.start_address().as_u64());

        // Deallocate and reallocate
        assert_eq!(Some(frame1), pmm.deallocate_frame(frame1));
        let frame3: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(frame1.start_address(), frame3.start_address());
    }

    #[test]
    fn test_sparse_with_preallocated_frames() {
        // Create a region with some frames already allocated
        let mut region = MemoryRegion::new(0x0000_0000, 8, FrameState::Free);
        // Pre-allocate frames 1, 3, 5
        region.frames_mut()[1] = FrameState::Allocated;
        region.frames_mut()[3] = FrameState::Allocated;
        region.frames_mut()[5] = FrameState::Allocated;

        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        // First free should be frame 0
        let frame1: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x0000, frame1.start_address().as_u64());

        // Next should be frame 2 (frame 1 is allocated)
        let frame2: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x2000, frame2.start_address().as_u64());
    }

    #[test]
    fn test_first_free_maintained_on_allocate() {
        // Test that allocate() correctly updates first_free
        let region = MemoryRegion::new(0, 10, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        // Initially, first_free should point to frame 0
        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        // Allocate frame 0
        let _frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        // first_free should now point to frame 1
        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 1);

        // Allocate frame 1
        let _frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        // first_free should now point to frame 2
        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 2);
    }

    #[test]
    fn test_first_free_maintained_on_deallocate() {
        // Test that deallocate() correctly updates first_free when deallocating before current first_free
        let region = MemoryRegion::new(0, 10, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        // Allocate several frames
        let frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        // first_free should be at frame 3 now
        assert_eq!(pmm.first_free.unwrap().frame_idx, 3);

        // Deallocate frame2 (which is before first_free)
        pmm.deallocate_frame(frame2).unwrap();

        // first_free should now point to frame2
        assert_eq!(pmm.first_free.unwrap().frame_idx, 1);

        // Deallocate frame1 (which is before current first_free)
        pmm.deallocate_frame(frame1).unwrap();

        // first_free should now point to frame1
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        // Deallocate frame3 (which is after first_free)
        pmm.deallocate_frame(frame3).unwrap();

        // first_free should still point to frame1
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);
    }

    #[test]
    fn test_first_free_all_frames_allocated() {
        // Test that first_free becomes None when all frames are allocated
        let region = MemoryRegion::new(0, 3, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        let _frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let _frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let _frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        // All frames allocated, first_free should be None
        assert!(pmm.first_free.is_none());

        // Try to allocate - should fail
        let result: Option<PhysFrame<Size4KiB>> = PhysicalFrameAllocator::allocate_frame(&mut pmm);
        assert!(result.is_none());
    }

    #[test]
    fn test_first_free_across_regions() {
        // Test that first_free correctly transitions between regions
        let region1 = MemoryRegion::new(0x0000_0000, 2, FrameState::Free);
        let region2 = MemoryRegion::new(0x1000_0000, 2, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region1, region2]);

        // first_free should be in region 0, frame 0
        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        // Allocate all frames from region 0
        let _frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let _frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        // first_free should now be in region 1, frame 0
        assert_eq!(pmm.first_free.unwrap().region_idx, 1);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        // Allocate from region 1
        let _frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        // first_free should be in region 1, frame 1
        assert_eq!(pmm.first_free.unwrap().region_idx, 1);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 1);
    }

    #[test]
    fn test_first_free_deallocate_to_earlier_region() {
        // Test that deallocating in an earlier region updates first_free
        let region1 = MemoryRegion::new(0x0000_0000, 2, FrameState::Free);
        let region2 = MemoryRegion::new(0x1000_0000, 2, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region1, region2]);

        // Allocate all frames from region 0
        let frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        // Allocate from region 1
        let _frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        // first_free should be in region 1
        assert_eq!(pmm.first_free.unwrap().region_idx, 1);

        // Deallocate a frame from region 0
        pmm.deallocate_frame(frame1).unwrap();

        // first_free should now be in region 0
        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        // Deallocate another frame from region 0 (after the current first_free)
        pmm.deallocate_frame(frame2).unwrap();

        // first_free should still be at region 0, frame 0
        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);
    }
}
