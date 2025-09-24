#![no_std]

extern crate alloc;

use alloc::collections::BTreeSet;

use log::debug;
pub use segment::*;
use thiserror::Error;
use x86_64::VirtAddr;

mod segment;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
#[error("segment already reserved")]
pub struct AlreadyReserved;

#[derive(Eq, PartialEq)]
pub struct VirtualMemoryManager {
    mem_start: VirtAddr,
    mem_size: u64,
    segments: BTreeSet<Segment>,
}

impl VirtualMemoryManager {
    #[must_use]
    pub fn new(mem_start: VirtAddr, mem_size: u64) -> Self {
        Self {
            mem_start,
            mem_size,
            segments: BTreeSet::default(),
        }
    }

    pub fn reserve(&mut self, n: usize) -> Option<Segment> {
        if n == 0 {
            return None;
        }

        let mut segment = Segment::new(self.mem_start, n as u64);
        while let Some(existing) = self.find_overlapping(&segment) {
            segment.start = existing.start + existing.len;
        }
        if self.mem_start + (self.mem_size - 1) < segment.start + (segment.len - 1) {
            return None;
        }

        self.segments.insert(segment);

        Some(Segment {
            start: segment.start,
            len: n as u64,
        })
    }

    pub fn release(&mut self, segment: Segment) -> bool {
        self.segments.remove(&segment)
    }

    /// Mark a segment as reserved, preventing it from being reserved again.
    ///
    /// # Errors
    ///
    /// Returns an error if the segment overlaps with an already reserved segment.
    pub fn mark_as_reserved(&mut self, segment: Segment) -> Result<(), AlreadyReserved> {
        if let Some(overlapping) = self.find_overlapping(&segment) {
            debug!("segment {segment:x?} overlaps with existing segment: {overlapping:x?}");
            return Err(AlreadyReserved);
        }
        self.segments.insert(segment);

        Ok(())
    }

    pub fn segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter()
    }

    fn find_overlapping(&self, segment: &Segment) -> Option<&Segment> {
        self.segments.iter().find(|existing| {
            segment.start <= existing.start + (existing.len - 1)
                && existing.start <= segment.start + (segment.len - 1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reserve_release() {
        let size = 50000_usize;
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0xabcd), size as u64);
        for n in (1..=size).step_by(713) {
            let segment = vmm
                .reserve(n)
                .unwrap_or_else(|| panic!("should be able to reserve segment of size {n}"));

            assert_eq!(segment.len, n as u64);

            vmm.release(segment);
        }
    }

    #[test]
    fn test_mark_as_used() {
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0xdeff), 400);
        let segment0 = Segment::new(VirtAddr::new(0xdeff), 100);
        let segment1 = Segment::new(VirtAddr::new(0xdeff + 100), 100);
        let segment1_5 = Segment::new(VirtAddr::new(0xdeff + 150), 100);
        let segment2 = Segment::new(VirtAddr::new(0xdeff + 200), 100);
        let segment3 = Segment::new(VirtAddr::new(0xdeff + 300), 100);

        vmm.mark_as_reserved(segment0).unwrap();
        vmm.mark_as_reserved(segment1).unwrap();
        vmm.mark_as_reserved(segment2).unwrap();
        vmm.mark_as_reserved(segment3).unwrap();

        assert_eq!(vmm.mark_as_reserved(segment1_5), Err(AlreadyReserved));

        vmm.release(segment1);
        assert_eq!(vmm.mark_as_reserved(segment1_5), Err(AlreadyReserved));

        vmm.mark_as_reserved(segment1).unwrap();
        vmm.release(segment2);
        assert_eq!(vmm.mark_as_reserved(segment1_5), Err(AlreadyReserved));

        vmm.release(segment1);
        vmm.mark_as_reserved(segment1_5).unwrap();
    }
}
