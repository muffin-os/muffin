#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PhysFrame, Size1GiB, Size2MiB, Size4KiB};
use x86_64::{PhysAddr, align_up};

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

#[derive(Debug, Copy, Clone)]
struct RegionFrameIndex {
    region_idx: usize,
    frame_idx: usize,
}

/// Tracks the state of every frame in a set of usable memory regions.
pub struct PhysicalMemoryManager {
    regions: Vec<MemoryRegion>,
    first_free: Option<RegionFrameIndex>,
}

impl PhysicalMemoryManager {
    /// Takes the regions as they are, including frames already marked
    /// `Allocated`.
    #[must_use]
    pub fn new(regions: Vec<MemoryRegion>) -> Self {
        let first_free = Self::find_first_free_internal(&regions);
        Self {
            regions,
            first_free,
        }
    }

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

    fn update_first_free(&mut self, start_region: usize, start_index: usize) {
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

        for (region_idx, region) in self.regions.iter().enumerate().skip(start_region + 1) {
            if let Some(frame_idx) = region.frames().iter().position(|&s| s == FrameState::Free) {
                self.first_free = Some(RegionFrameIndex {
                    region_idx,
                    frame_idx,
                });
                return;
            }
        }

        self.first_free = None;
    }

    fn allocate_frames_impl<S: PageSize>(
        &mut self,
        n: usize,
    ) -> Option<PhysFrameRangeInclusive<S>> {
        let small_frames_per_frame = (S::SIZE / Size4KiB::SIZE) as usize;
        let small_frame_count = n * small_frames_per_frame;

        let ff = self.first_free?;

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

            let Some(start_addr) = region.frame_address(search_start) else {
                continue;
            };
            let skipped = (align_up(start_addr, S::SIZE) - start_addr) / Size4KiB::SIZE;

            let mut current_start = search_start + skipped as usize;
            while current_start + small_frame_count <= region.len() {
                let all_free = region.frames()[current_start..current_start + small_frame_count]
                    .iter()
                    .all(|&state| state == FrameState::Free);

                if all_free {
                    let frame_start_idx = current_start;
                    let frame_end_idx = current_start + small_frame_count - 1;

                    let start_addr = self.regions[region_idx]
                        .frame_address(frame_start_idx)
                        .expect("frame_address(frame_start_idx) should succeed: frame exists and is free");
                    let last_page_start_idx = frame_start_idx + (n - 1) * small_frames_per_frame;
                    let end_addr = self.regions[region_idx]
                        .frame_address(last_page_start_idx)
                        .expect("frame_address(last_page_start_idx) should succeed: frame exists and is free");

                    self.regions[region_idx].frames_mut()[frame_start_idx..=frame_end_idx]
                        .fill(FrameState::Allocated);

                    if region_idx == ff.region_idx && frame_start_idx <= ff.frame_idx {
                        self.update_first_free(region_idx, frame_end_idx + 1);
                    }

                    return Some(PhysFrameRangeInclusive {
                        start: PhysFrame::from_start_address(PhysAddr::new(start_addr))
                            .expect("start address must be aligned to page size"),
                        end: PhysFrame::from_start_address(PhysAddr::new(end_addr))
                            .expect("end address must be aligned to page size"),
                    });
                }

                current_start += small_frames_per_frame;
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

    /// Returns `None` if any frame in the range was not allocated. The other
    /// frames are still freed.
    fn deallocate_frames(
        &mut self,
        range: PhysFrameRangeInclusive<S>,
    ) -> Option<PhysFrameRangeInclusive<S>> {
        let mut all_freed = true;
        for frame in range {
            all_freed &= self.deallocate_frame(frame).is_some();
        }
        all_freed.then_some(range)
    }
}

impl PhysicalFrameAllocator<Size4KiB> for PhysicalMemoryManager {
    fn allocate_frames(&mut self, n: usize) -> Option<PhysFrameRangeInclusive<Size4KiB>> {
        self.allocate_frames_impl(n)
    }

    fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) -> Option<PhysFrame<Size4KiB>> {
        let addr = frame.start_address().as_u64();

        let loc = Self::find_frame_location(&self.regions, addr);
        debug_assert!(loc.is_some(), "deallocating unmanaged frame {frame:?}");
        let loc = loc?;

        let state = self.regions[loc.region_idx].frames()[loc.frame_idx];
        debug_assert!(state == FrameState::Allocated, "double free of {frame:?}");
        if state == FrameState::Allocated {
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
        let mut all_freed = true;
        for i in 0..(Size2MiB::SIZE / Size4KiB::SIZE) as usize {
            let frame = PhysFrame::<Size4KiB>::containing_address(
                frame.start_address() + (i as u64 * Size4KiB::SIZE),
            );
            all_freed &= self.deallocate_frame(frame).is_some();
        }

        all_freed.then_some(frame)
    }
}

impl PhysicalFrameAllocator<Size1GiB> for PhysicalMemoryManager {
    fn allocate_frames(&mut self, n: usize) -> Option<PhysFrameRangeInclusive<Size1GiB>> {
        self.allocate_frames_impl(n)
    }

    fn deallocate_frame(&mut self, frame: PhysFrame<Size1GiB>) -> Option<PhysFrame<Size1GiB>> {
        let mut all_freed = true;
        for i in 0..(Size1GiB::SIZE / Size2MiB::SIZE) as usize {
            let frame = PhysFrame::<Size2MiB>::containing_address(
                frame.start_address() + (i as u64 * Size2MiB::SIZE),
            );
            all_freed &= self.deallocate_frame(frame).is_some();
        }

        all_freed.then_some(frame)
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
        let small_frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

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
        let small_frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        let frame1: PhysFrame<Size1GiB> = pmm.allocate_frame().unwrap();
        assert_eq!(1024 * 1024 * 1024, frame1.start_address().as_u64());

        assert_eq!(Option::<PhysFrame<Size1GiB>>::None, pmm.allocate_frame());
        let small_frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(Some(small_frame1), pmm.deallocate_frame(small_frame1));
        assert_eq!(Some(frame1), pmm.deallocate_frame(frame1));
        assert_eq!(Some(small_frame2), pmm.deallocate_frame(small_frame2));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "double free")]
    fn test_double_free_panics() {
        let region = MemoryRegion::new(0, 4, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);
        let frame: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        assert_eq!(Some(frame), pmm.deallocate_frame(frame));
        pmm.deallocate_frame(frame);
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn test_deallocate_frames_continues_past_double_free() {
        let region = MemoryRegion::new(0, 4, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);
        let range: PhysFrameRangeInclusive<Size4KiB> = pmm.allocate_frames(4).unwrap();
        assert_eq!(Some(range.start + 1), pmm.deallocate_frame(range.start + 1));

        assert_eq!(None, pmm.deallocate_frames(range));
        assert_eq!(Some(range), pmm.allocate_frames(4));
    }

    #[test]
    fn test_sparse_multiple_regions() {
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
        let region1 = MemoryRegion::new(0x0000_0000, 4, FrameState::Free);
        let region2 = MemoryRegion::new(0x1000_0000, 4, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region1, region2]);

        let frame1: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x0000, frame1.start_address().as_u64());

        let frame2: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x1000, frame2.start_address().as_u64());

        assert_eq!(Some(frame1), pmm.deallocate_frame(frame1));
        let frame3: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(frame1.start_address(), frame3.start_address());
    }

    #[test]
    fn test_sparse_with_preallocated_frames() {
        let mut region = MemoryRegion::new(0x0000_0000, 8, FrameState::Free);
        region.frames_mut()[1] = FrameState::Allocated;
        region.frames_mut()[3] = FrameState::Allocated;
        region.frames_mut()[5] = FrameState::Allocated;

        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        let frame1: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x0000, frame1.start_address().as_u64());

        let frame2: PhysFrame<Size4KiB> = PhysicalFrameAllocator::allocate_frame(&mut pmm).unwrap();
        assert_eq!(0x2000, frame2.start_address().as_u64());
    }

    #[test]
    fn test_first_free_maintained_on_allocate() {
        let region = MemoryRegion::new(0, 10, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        let _frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 1);

        let _frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 2);
    }

    #[test]
    fn test_first_free_maintained_on_deallocate() {
        let region = MemoryRegion::new(0, 10, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        let frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(pmm.first_free.unwrap().frame_idx, 3);

        pmm.deallocate_frame(frame2).unwrap();

        assert_eq!(pmm.first_free.unwrap().frame_idx, 1);

        pmm.deallocate_frame(frame1).unwrap();

        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        pmm.deallocate_frame(frame3).unwrap();

        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);
    }

    #[test]
    fn test_first_free_all_frames_allocated() {
        let region = MemoryRegion::new(0, 3, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        let _frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let _frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let _frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert!(pmm.first_free.is_none());

        let result: Option<PhysFrame<Size4KiB>> = PhysicalFrameAllocator::allocate_frame(&mut pmm);
        assert!(result.is_none());
    }

    #[test]
    fn test_first_free_across_regions() {
        let region1 = MemoryRegion::new(0x0000_0000, 2, FrameState::Free);
        let region2 = MemoryRegion::new(0x1000_0000, 2, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region1, region2]);

        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        let _frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let _frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(pmm.first_free.unwrap().region_idx, 1);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        let _frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(pmm.first_free.unwrap().region_idx, 1);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 1);
    }

    #[test]
    fn test_first_free_deallocate_to_earlier_region() {
        let region1 = MemoryRegion::new(0x0000_0000, 2, FrameState::Free);
        let region2 = MemoryRegion::new(0x1000_0000, 2, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region1, region2]);

        let frame1: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        let frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        let _frame3: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();

        assert_eq!(pmm.first_free.unwrap().region_idx, 1);

        pmm.deallocate_frame(frame1).unwrap();

        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);

        pmm.deallocate_frame(frame2).unwrap();

        assert_eq!(pmm.first_free.unwrap().region_idx, 0);
        assert_eq!(pmm.first_free.unwrap().frame_idx, 0);
    }

    #[test]
    fn test_allocate_2mib_with_misaligned_first_free() {
        let region = MemoryRegion::new(0, 1024, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        let small_frame: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        assert_eq!(0, small_frame.start_address().as_u64());

        assert_eq!(pmm.first_free.unwrap().frame_idx, 1);

        let large_frame: PhysFrame<Size2MiB> = pmm.allocate_frame().unwrap();

        assert_eq!(512 * 4096, large_frame.start_address().as_u64());

        let small_frame2: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        assert_eq!(4096, small_frame2.start_address().as_u64());
    }

    #[test]
    fn test_allocate_2mib_from_misaligned_region() {
        let region = MemoryRegion::new(0x1000, 1024, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        let large_frame: PhysFrame<Size2MiB> = pmm.allocate_frame().unwrap();
        assert_eq!(0x200000, large_frame.start_address().as_u64());

        let small_frame: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        assert_eq!(0x1000, small_frame.start_address().as_u64());
    }

    #[cfg(not(miri))] // this takes a while
    #[test]
    fn test_allocate_1gib_from_misaligned_region() {
        let frames_before_aligned = (0x40000000 - 0x200000) / 4096;
        let frames_for_1gib = (1024 * 1024 * 1024) / 4096;
        let total_frames = frames_before_aligned + frames_for_1gib;

        let region = MemoryRegion::new(0x200000, total_frames as usize, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        let large_frame: PhysFrame<Size1GiB> = pmm.allocate_frame().unwrap();
        assert_eq!(0x40000000, large_frame.start_address().as_u64());

        let small_frame: PhysFrame<Size4KiB> = pmm.allocate_frame().unwrap();
        assert_eq!(0x200000, small_frame.start_address().as_u64());
    }

    #[test]
    fn test_allocate_2mib_from_perfectly_aligned_region() {
        let region = MemoryRegion::new(0x200000, 1024, FrameState::Free);
        let mut pmm = PhysicalMemoryManager::new(vec![region]);

        let large_frame: PhysFrame<Size2MiB> = pmm.allocate_frame().unwrap();
        assert_eq!(0x200000, large_frame.start_address().as_u64());
    }
}
