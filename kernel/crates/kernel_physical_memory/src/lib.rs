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

/// A physical memory manager that keeps track of the state of each frame in the
/// system.
pub struct PhysicalMemoryManager {
    frames: Vec<FrameState>,
    first_free: Option<usize>,
}

impl PhysicalMemoryManager {
    #[must_use]
    pub fn new(frames: Vec<FrameState>) -> Self {
        let first_free = frames.iter().position(|&state| state == FrameState::Free);
        Self { frames, first_free }
    }

    fn allocate_frames_impl<S: PageSize>(
        &mut self,
        n: usize,
    ) -> Option<PhysFrameRangeInclusive<S>> {
        // search for the first frame index that aligns with the page size, starting
        // at `first_free`
        let (start_index, small_frames_per_frame) = {
            let index_align = (S::SIZE / Size4KiB::SIZE) as usize;

            let first_free = self.first_free?;
            // align_up first_free to index_align
            let start_index = first_free + (index_align - first_free % index_align) % index_align;

            (start_index, index_align)
        };

        let small_frame_count = n * small_frames_per_frame;
        let first_frame_index = self
            .frames
            .chunks_exact(small_frame_count)
            .skip(start_index / small_frames_per_frame)
            .position(|window| window.iter().all(|&state| state == FrameState::Free))?
            * small_frame_count
            + start_index;
        let last_small_frame_index = first_frame_index + small_frame_count - 1;

        // mark the frames as allocated
        self.frames[first_frame_index..=last_small_frame_index].fill(FrameState::Allocated);

        if start_index == self.first_free? {
            // we started at the first free frame, so we need to find the next free frame,
            // otherwise we may have skipped frames due to alignment, and we can keep the
            // last `first_free`
            self.first_free = self
                .frames
                .iter()
                .skip(last_small_frame_index)
                .position(|&state| state == FrameState::Free);
        }

        // align_down last_small_frame_index to small_frames_per_frame
        let last_frame_index =
            last_small_frame_index / small_frames_per_frame * small_frames_per_frame;

        Some(PhysFrameRangeInclusive {
            start: self
                .index_to_frame(first_frame_index)
                .expect("start index should be aligned and within bounds"),
            end: self
                .index_to_frame(last_frame_index)
                .expect("end index should be aligned and within bounds"),
        })
    }

    /// Converts a 4KiB frame index to a physical frame, if that frame index
    /// aligns with the page size [`S`] and the index is within the bounds of
    /// the frame list.
    ///
    /// For example, if [`S`] is [`Size4KiB`], the frame index must be a multiple
    /// of 1, if [`S`] is [`Size2MiB`], the frame index must be a multiple of 512
    /// and so on.
    ///
    /// Calling this function with an index of 2 (address 0x2000) and [`S`] being
    /// [`Size2MiB`] will return [`None`], since frame index 2 is not 2MiB aligned.
    fn index_to_frame<S: PageSize>(&self, index: usize) -> Option<PhysFrame<S>> {
        if index >= self.frames.len() {
            return None;
        }

        let addr = index as u64 * Size4KiB::SIZE;
        // address must be aligned to [`S`]'s page size
        if addr.is_multiple_of(S::SIZE) {
            Some(PhysFrame::containing_address(PhysAddr::new(addr)))
        } else {
            None
        }
    }

    fn frame_to_index<S: PageSize>(&self, frame: PhysFrame<S>) -> Option<usize> {
        let addr = frame.start_address().as_u64();
        let index = (addr / Size4KiB::SIZE) as usize;
        if index < self.frames.len() {
            Some(index)
        } else {
            None
        }
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
        let index = self.frame_to_index(frame)?;
        if self.frames[index] == FrameState::Allocated {
            self.frames[index] = FrameState::Free;
            if self.first_free.is_some_and(|v| index < v) || self.first_free.is_none() {
                self.first_free = Some(index);
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
        assert_eq!(4, pmm.frames.len());
        assert_eq!(&states[..], &pmm.frames[..]);
    }

    #[test]
    fn test_new_trailing_unusable() {
        let states = vec![FrameState::Unusable, FrameState::Free, FrameState::Unusable];
        let pmm = PhysicalMemoryManager::new(states.clone());
        assert_eq!(3, pmm.frames.len());
        assert_eq!(&states[..], &pmm.frames[..]);
    }

    #[test]
    fn test_new_no_frames() {
        let states = vec![];
        let pmm = PhysicalMemoryManager::new(states.clone());
        assert!(pmm.frames.is_empty());
    }

    #[test]
    fn test_allocate_deallocate_4kib() {
        let mut pmm = PhysicalMemoryManager::new(vec![FrameState::Free; 4]);
        assert_eq!(4, pmm.frames.len());
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
        assert_eq!(4, pmm.frames.len());
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
}
