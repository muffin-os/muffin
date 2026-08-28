#![no_std]

extern crate alloc;

use alloc::collections::BTreeSet;

pub use segment::*;
use thiserror::Error;
use tracing::debug;
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
        let len = n as u64;
        if len == 0 || len > self.mem_size {
            return None;
        }
        // Inclusive bound. An exclusive end overflows for a manager that
        // reaches the top of the address space.
        let mem_last = self.mem_start.as_u64() as u128 + (self.mem_size as u128 - 1);

        // Probe arithmetic is u128 because a candidate range can exceed the
        // manager's bounds before it is rejected, and such an address fails
        // VirtAddr's canonicality check.
        let mut start = self.mem_start.as_u64() as u128;
        loop {
            if start + (len as u128 - 1) > mem_last {
                return None;
            }
            match self.find_overlapping_at(start, len) {
                Some(existing) => {
                    start = existing.start.as_u64() as u128 + existing.len as u128;
                }
                None => break,
            }
        }

        let segment = Segment::new(VirtAddr::new(start as u64), len);
        self.segments.insert(segment);
        Some(segment)
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
        self.find_overlapping_at(segment.start.as_u64() as u128, segment.len)
    }

    fn find_overlapping_at(&self, start: u128, len: u64) -> Option<&Segment> {
        if len == 0 {
            return None;
        }
        let last = start + (len as u128 - 1);
        self.segments.iter().find(|existing| {
            let existing_start = existing.start.as_u64() as u128;
            existing.len != 0
                && start <= existing_start + (existing.len as u128 - 1)
                && existing_start <= last
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

    #[test]
    fn reserve_larger_than_manager() {
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0x1_0000_0000), 0x7F00_0000_0000);
        assert_eq!(
            vmm.reserve(1 << 47),
            None,
            "request exceeding the managed range must be refused"
        );
    }

    #[test]
    fn reserve_probe_past_manager_end() {
        // The probe past the reserved page crosses the canonical boundary.
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0x7FFF_FFFF_0000), 0x10000);
        vmm.mark_as_reserved(Segment::new(VirtAddr::new(0x7FFF_FFFF_8000), 0x1000))
            .unwrap();
        assert_eq!(
            vmm.reserve(0x9000),
            None,
            "no gap fits the request, refusal must not panic"
        );
    }

    #[test]
    fn zero_length_segments_never_overlap() {
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0x1000), 0x10000);
        vmm.mark_as_reserved(Segment::new(VirtAddr::new(0x2000), 0x1000))
            .unwrap();
        vmm.mark_as_reserved(Segment::new(VirtAddr::new(0x2000), 0))
            .unwrap();
        let segment = vmm.reserve(0x1000);
        assert!(
            segment.is_some(),
            "zero-length entries must not block reservations"
        );
    }
}
