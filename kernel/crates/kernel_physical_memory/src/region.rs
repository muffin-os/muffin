use alloc::vec::Vec;

use x86_64::structures::paging::{PageSize, Size4KiB};

use crate::FrameState;

/// Represents a contiguous region of usable physical memory.
#[derive(Debug, Clone)]
pub struct MemoryRegion {
    /// Starting physical address of the region (must be 4KiB aligned)
    base_addr: u64,
    /// Frame states for this region (indexed by frame offset from base_addr)
    frames: Vec<FrameState>,
}

impl MemoryRegion {
    pub fn new(base_addr: u64, num_frames: usize, initial_state: FrameState) -> Self {
        Self {
            base_addr,
            frames: alloc::vec![initial_state; num_frames],
        }
    }

    /// Creates a new MemoryRegion with custom frame states
    pub fn with_frames(base_addr: u64, frames: Vec<FrameState>) -> Self {
        Self { base_addr, frames }
    }

    /// Returns the base address of this region.
    pub fn base_addr(&self) -> u64 {
        self.base_addr
    }

    /// Returns the number of frames in this region.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Returns true if this region contains no frames.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Returns a reference to the frame states.
    pub(crate) fn frames(&self) -> &[FrameState] {
        &self.frames
    }

    /// Returns a mutable reference to the frame states.
    pub fn frames_mut(&mut self) -> &mut [FrameState] {
        &mut self.frames
    }

    /// Returns the frame index within this region for the given physical address,
    /// or None if the address is not in this region.
    pub fn frame_index(&self, addr: u64) -> Option<usize> {
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
    pub(crate) fn frame_address(&self, index: usize) -> Option<u64> {
        if index < self.frames.len() {
            Some(self.base_addr + (index as u64 * Size4KiB::SIZE))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use x86_64::structures::paging::PageSize;

    use super::*;

    #[test]
    fn test_new_region() {
        let region = MemoryRegion::new(0x1000, 10, FrameState::Free);
        assert_eq!(region.base_addr(), 0x1000);
        assert_eq!(region.len(), 10);
        assert!(region.frames().iter().all(|&s| s == FrameState::Free));
    }

    #[test]
    fn test_frame_index() {
        let region = MemoryRegion::new(0x1000, 10, FrameState::Free);

        // Address below region
        assert_eq!(region.frame_index(0x0), None);

        // First frame
        assert_eq!(region.frame_index(0x1000), Some(0));

        // Second frame
        assert_eq!(region.frame_index(0x2000), Some(1));

        // Last frame
        assert_eq!(region.frame_index(0x1000 + 9 * Size4KiB::SIZE), Some(9));

        // Beyond region
        assert_eq!(region.frame_index(0x1000 + 10 * Size4KiB::SIZE), None);
    }

    #[test]
    fn test_frame_address() {
        let region = MemoryRegion::new(0x1000, 10, FrameState::Free);

        assert_eq!(region.frame_address(0), Some(0x1000));
        assert_eq!(region.frame_address(1), Some(0x1000 + Size4KiB::SIZE));
        assert_eq!(region.frame_address(9), Some(0x1000 + 9 * Size4KiB::SIZE));
        assert_eq!(region.frame_address(10), None);
    }

    #[test]
    fn test_frames_mut() {
        let mut region = MemoryRegion::new(0x0, 5, FrameState::Free);
        region.frames_mut()[2] = FrameState::Allocated;

        assert_eq!(region.frames()[0], FrameState::Free);
        assert_eq!(region.frames()[2], FrameState::Allocated);
        assert_eq!(region.frames()[4], FrameState::Free);
    }
}
