#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use x86_64::PhysAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PhysFrame, Size1GiB, Size2MiB, Size4KiB};

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

/// Represents a contiguous region of usable physical memory.
#[derive(Debug, Clone)]
struct MemoryRegion {
    /// Starting physical address of the region (must be 4KiB aligned)
    base_addr: u64,
    /// Frame states for this region (indexed by frame offset from base_addr)
    frames: Vec<FrameState>,
}

impl MemoryRegion {
    fn new(base_addr: u64, num_frames: usize, initial_state: FrameState) -> Self {
        Self {
            base_addr,
            frames: alloc::vec![initial_state; num_frames],
        }
    }

    /// Returns the frame index within this region for the given physical address,
    /// or None if the address is not in this region.
    fn frame_index(&self, addr: u64) -> Option<usize> {
        if addr < self.base_addr {
            return None;
        }
        let offset = addr - self.base_addr;
        let index = (offset / Size4KiB::SIZE) as usize;
        if index < self.frames.len() {
            Some(index)
        } else {
            None
        }
    }

    /// Returns the physical address for the frame at the given index within this region.
    fn frame_address(&self, index: usize) -> Option<u64> {
        if index < self.frames.len() {
            Some(self.base_addr + (index as u64 * Size4KiB::SIZE))
        } else {
            None
        }
    }
}

/// A physical memory manager that keeps track of the state of each frame in the
/// system. Supports both dense (legacy) and sparse representations.
pub struct PhysicalMemoryManager {
    regions: Vec<MemoryRegion>,
    first_free_region: Option<usize>,
    first_free_index: Option<usize>,
}

impl PhysicalMemoryManager {
    /// Creates a new manager from a dense vector of frame states (legacy mode).
    /// This creates a single region starting at address 0.
    #[must_use]
    pub fn new(frames: Vec<FrameState>) -> Self {
        if frames.is_empty() {
            return Self {
                regions: Vec::new(),
                first_free_region: None,
                first_free_index: None,
            };
        }

        let region = MemoryRegion {
            base_addr: 0,
            frames,
        };
        
        let (first_free_region, first_free_index) = region
            .frames
            .iter()
            .position(|&state| state == FrameState::Free)
            .map(|idx| (Some(0), Some(idx)))
            .unwrap_or((None, None));

        Self {
            regions: alloc::vec![region],
            first_free_region,
            first_free_index,
        }
    }

    /// Creates a new sparse manager from usable memory regions.
    /// This is more memory-efficient as it only tracks usable regions.
    /// 
    /// # Arguments
    /// * `usable_regions` - Iterator of (base_address, length_in_bytes) tuples representing usable memory regions
    /// * `allocated_frames` - Iterator of already-allocated physical frame numbers (relative to start of all frames)
    #[must_use]
    pub fn new_sparse<I, A>(usable_regions: I, allocated_frames: A) -> Self
    where
        I: IntoIterator<Item = (u64, u64)>,
        A: IntoIterator<Item = u64>,
    {
        let mut regions: Vec<MemoryRegion> = usable_regions
            .into_iter()
            .map(|(base, length)| {
                let num_frames = (length / Size4KiB::SIZE) as usize;
                MemoryRegion::new(base, num_frames, FrameState::Free)
            })
            .collect();

        if regions.is_empty() {
            return Self {
                regions: Vec::new(),
                first_free_region: None,
                first_free_index: None,
            };
        }

        // Mark allocated frames
        for frame_addr in allocated_frames {
            if let Some((region_idx, local_idx)) = Self::find_frame_location(&regions, frame_addr) {
                regions[region_idx].frames[local_idx] = FrameState::Allocated;
            }
        }

        // Find first free frame
        let (first_free_region, first_free_index) = Self::find_first_free(&regions);

        Self {
            regions,
            first_free_region,
            first_free_index,
        }
    }

    /// Find the region and local index for a given physical address
    fn find_frame_location(regions: &[MemoryRegion], addr: u64) -> Option<(usize, usize)> {
        for (region_idx, region) in regions.iter().enumerate() {
            if let Some(local_idx) = region.frame_index(addr) {
                return Some((region_idx, local_idx));
            }
        }
        None
    }

    /// Find the first free frame across all regions
    fn find_first_free(regions: &[MemoryRegion]) -> (Option<usize>, Option<usize>) {
        for (region_idx, region) in regions.iter().enumerate() {
            if let Some(local_idx) = region.frames.iter().position(|&s| s == FrameState::Free) {
                return (Some(region_idx), Some(local_idx));
            }
        }
        (None, None)
    }

    /// Update the first_free pointers starting from a given position
    fn update_first_free(&mut self, start_region: usize, start_index: usize) {
        // Check if there are more free frames in the current region
        if let Some(region) = self.regions.get(start_region) {
            if let Some(idx) = region.frames[start_index..].iter().position(|&s| s == FrameState::Free) {
                self.first_free_region = Some(start_region);
                self.first_free_index = Some(start_index + idx);
                return;
            }
        }

        // Search subsequent regions
        for (region_idx, region) in self.regions.iter().enumerate().skip(start_region + 1) {
            if let Some(idx) = region.frames.iter().position(|&s| s == FrameState::Free) {
                self.first_free_region = Some(region_idx);
                self.first_free_index = Some(idx);
                return;
            }
        }

        // No free frames found
        self.first_free_region = None;
        self.first_free_index = None;
    }

    fn allocate_frames_impl<S: PageSize>(
        &mut self,
        n: usize,
    ) -> Option<PhysFrameRangeInclusive<S>> {
        let small_frames_per_frame = (S::SIZE / Size4KiB::SIZE) as usize;
        let small_frame_count = n * small_frames_per_frame;

        let start_region = self.first_free_region?;
        let start_index = self.first_free_index?;

        // Search for contiguous free frames within regions
        // Note: We don't search across region boundaries for simplicity
        for region_idx in start_region..self.regions.len() {
            let search_start = if region_idx == start_region { start_index } else { 0 };
            
            let region = &self.regions[region_idx];
            if search_start >= region.frames.len() {
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
            while current_start + small_frame_count <= region.frames.len() {
                // Check if we have enough contiguous free frames
                let all_free = region.frames[current_start..current_start + small_frame_count]
                    .iter()
                    .all(|&state| state == FrameState::Free);
                
                if all_free {
                    let frame_start_idx = current_start;
                    let frame_end_idx = current_start + small_frame_count - 1;

                    // Get the physical addresses before mutating
                    let start_addr = self.regions[region_idx].frame_address(frame_start_idx)?;
                    let end_addr_idx = frame_end_idx / small_frames_per_frame * small_frames_per_frame;
                    let end_addr = self.regions[region_idx].frame_address(end_addr_idx)?;

                    // Mark frames as allocated
                    self.regions[region_idx].frames[frame_start_idx..=frame_end_idx].fill(FrameState::Allocated);

                    // Update first_free pointers
                    if region_idx == start_region && frame_start_idx <= start_index {
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

    /// Converts a physical address to a physical frame, if the address is 4KiB aligned,
    /// aligns with the page size [`S`], and is within a usable region.
    fn addr_to_frame<S: PageSize>(&self, addr: u64) -> Option<PhysFrame<S>> {
        // Check if address is aligned to S's page size
        if !addr.is_multiple_of(S::SIZE) {
            return None;
        }

        // Check if address is in a usable region
        for region in &self.regions {
            if let Some(_) = region.frame_index(addr) {
                return Some(PhysFrame::containing_address(PhysAddr::new(addr)));
            }
        }

        None
    }

    /// Converts a 4KiB frame index (from legacy dense representation) to a physical frame.
    /// This is kept for backward compatibility with tests.
    fn index_to_frame<S: PageSize>(&self, index: usize) -> Option<PhysFrame<S>> {
        let addr = index as u64 * Size4KiB::SIZE;
        self.addr_to_frame(addr)
    }

    /// Converts a physical frame to an address and checks if it's in a usable region.
    fn frame_to_addr<S: PageSize>(&self, frame: PhysFrame<S>) -> Option<u64> {
        let addr = frame.start_address().as_u64();
        
        // Check if frame is in a usable region
        for region in &self.regions {
            if region.frame_index(addr).is_some() {
                return Some(addr);
            }
        }

        None
    }

    /// Converts a physical frame to an index (for legacy compatibility).
    fn frame_to_index<S: PageSize>(&self, frame: PhysFrame<S>) -> Option<usize> {
        let addr = frame.start_address().as_u64();
        
        // For legacy mode (single region starting at 0), return simple index
        if self.regions.len() == 1 && self.regions[0].base_addr == 0 {
            let index = (addr / Size4KiB::SIZE) as usize;
            if index < self.regions[0].frames.len() {
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
        let (region_idx, local_idx) = Self::find_frame_location(&self.regions, addr)?;
        
        if self.regions[region_idx].frames[local_idx] == FrameState::Allocated {
            self.regions[region_idx].frames[local_idx] = FrameState::Free;
            
            // Update first_free if this is before the current first_free
            let is_before_first_free = match (self.first_free_region, self.first_free_index) {
                (Some(ff_region), Some(ff_index)) => {
                    region_idx < ff_region || (region_idx == ff_region && local_idx < ff_index)
                }
                _ => true,
            };
            
            if is_before_first_free {
                self.first_free_region = Some(region_idx);
                self.first_free_index = Some(local_idx);
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
        let pmm = PhysicalMemoryManager::new(states.clone());
        assert_eq!(1, pmm.regions.len());
        assert_eq!(4, pmm.regions[0].frames.len());
        assert_eq!(&states[..], &pmm.regions[0].frames[..]);
    }

    #[test]
    fn test_new_trailing_unusable() {
        let states = vec![FrameState::Unusable, FrameState::Free, FrameState::Unusable];
        let pmm = PhysicalMemoryManager::new(states.clone());
        assert_eq!(1, pmm.regions.len());
        assert_eq!(3, pmm.regions[0].frames.len());
        assert_eq!(&states[..], &pmm.regions[0].frames[..]);
    }

    #[test]
    fn test_new_no_frames() {
        let states = vec![];
        let pmm = PhysicalMemoryManager::new(states.clone());
        assert!(pmm.regions.is_empty());
    }

    #[test]
    fn test_allocate_deallocate_4kib() {
        let mut pmm = PhysicalMemoryManager::new(vec![FrameState::Free; 4]);
        assert_eq!(1, pmm.regions.len());
        assert_eq!(4, pmm.regions[0].frames.len());
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
        assert_eq!(4, pmm.regions[0].frames.len());
    }

    #[test]
    fn test_allocate_deallocate_2mib() {
        let mut pmm = PhysicalMemoryManager::new(vec![FrameState::Free; 1024]); // 4MiB
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
        let mut pmm = PhysicalMemoryManager::new(vec![FrameState::Free; 512 * 512 * 2]); // 2GiB
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
    fn test_new_sparse() {
        // Create sparse manager with two regions
        let regions = vec![
            (0x0000_0000, 4 * Size4KiB::SIZE),         // 4 frames at 0
            (0x1000_0000, 4 * Size4KiB::SIZE),         // 4 frames at 256MB
        ];
        let pmm = PhysicalMemoryManager::new_sparse(regions, vec![]);
        
        assert_eq!(2, pmm.regions.len());
        assert_eq!(4, pmm.regions[0].frames.len());
        assert_eq!(4, pmm.regions[1].frames.len());
        assert_eq!(0x0000_0000, pmm.regions[0].base_addr);
        assert_eq!(0x1000_0000, pmm.regions[1].base_addr);
    }

    #[test]
    fn test_sparse_allocate_deallocate() {
        // Create sparse manager with gaps
        let regions = vec![
            (0x0000_0000, 4 * Size4KiB::SIZE),
            (0x1000_0000, 4 * Size4KiB::SIZE),
        ];
        let mut pmm = PhysicalMemoryManager::new_sparse(regions, vec![]);
        
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
    fn test_sparse_with_allocated_frames() {
        let regions = vec![
            (0x0000_0000, 8 * Size4KiB::SIZE),
        ];
        // Pre-allocate frames 1, 3, 5
        let allocated = vec![0x1000, 0x3000, 0x5000];
        let mut pmm = PhysicalMemoryManager::new_sparse(regions, allocated);
        
        // First free should be frame 0
        let frame1: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x0000, frame1.start_address().as_u64());
        
        // Next should be frame 2 (frame 1 is allocated)
        let frame2: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x2000, frame2.start_address().as_u64());
    }
}
