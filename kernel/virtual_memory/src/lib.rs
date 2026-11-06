#![no_std]

extern crate alloc;

use alloc::collections::BTreeSet;

pub use segment::*;
use thiserror::Error;
use tracing::debug;
use x86_64::VirtAddr;
use x86_64::structures::paging::{PageSize, Size4KiB};

mod segment;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
#[error("segment already reserved")]
pub struct AlreadyReserved;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum SplitSegmentError {
    #[error("segment not reserved")]
    NotReserved,
    #[error("split point is not a page boundary strictly inside the segment")]
    InvalidSplitPoint,
}

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

    pub fn reserve_bytes(&mut self, len: u64) -> Option<Segment> {
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
                    start = existing.last().as_u64() as u128 + 1;
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

    /// This method replaces the reserved `segment` with the two segments that
    /// meet at `at`. A failed split leaves the reservation unchanged.
    ///
    /// # Errors
    /// This method returns `InvalidSplitPoint` when `at` is not page aligned or
    /// not strictly inside `segment`. It returns `NotReserved` when `segment`
    /// is not reserved exactly as given.
    pub fn split(
        &mut self,
        segment: Segment,
        at: VirtAddr,
    ) -> Result<(Segment, Segment), SplitSegmentError> {
        let low_len = at
            .as_u64()
            .checked_sub(segment.start.as_u64())
            .filter(|&len| len > 0 && len < segment.len && at.is_aligned(Size4KiB::SIZE))
            .ok_or(SplitSegmentError::InvalidSplitPoint)?;
        if !self.segments.remove(&segment) {
            return Err(SplitSegmentError::NotReserved);
        }
        let low = Segment::new(segment.start, low_len);
        let high = Segment::new(at, segment.len - low_len);
        self.segments.insert(low);
        self.segments.insert(high);
        Ok((low, high))
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
            existing.len != 0
                && start <= existing.last().as_u64() as u128
                && existing.start.as_u64() as u128 <= last
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn test_reserve_release() {
        let size = 50000_usize;
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0xabcd), size as u64);
        for n in (1..=size).step_by(713) {
            let segment = vmm
                .reserve_bytes(n as u64)
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
            vmm.reserve_bytes(1 << 47),
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
            vmm.reserve_bytes(0x9000),
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
        let segment = vmm.reserve_bytes(0x1000);
        assert!(
            segment.is_some(),
            "zero-length entries must not block reservations"
        );
    }

    #[test]
    fn split_replaces_the_segment_with_two_halves() {
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0x1000), 0x10000);
        let segment = Segment::new(VirtAddr::new(0x2000), 0x4000);
        vmm.mark_as_reserved(segment).unwrap();

        let (low, high) = vmm.split(segment, VirtAddr::new(0x3000)).unwrap();

        assert_eq!(low, Segment::new(VirtAddr::new(0x2000), 0x1000));
        assert_eq!(high, Segment::new(VirtAddr::new(0x3000), 0x3000));
        assert_eq!(vmm.segments().copied().collect::<Vec<_>>(), [low, high]);
        assert!(vmm.release(low), "each half is a reservation of its own");
        assert_eq!(
            vmm.mark_as_reserved(high),
            Err(AlreadyReserved),
            "the high half stays reserved"
        );
    }

    #[test]
    fn split_at_a_boundary_or_off_a_page_is_rejected() {
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0x1000), 0x10000);
        let segment = Segment::new(VirtAddr::new(0x2000), 0x4000);
        vmm.mark_as_reserved(segment).unwrap();

        for at in [0x1000, 0x2000, 0x6000, 0x7000, 0x3001] {
            assert_eq!(
                vmm.split(segment, VirtAddr::new(at)),
                Err(SplitSegmentError::InvalidSplitPoint),
                "split at {at:#x}"
            );
        }
        assert_eq!(
            vmm.segments().copied().collect::<Vec<_>>(),
            [segment],
            "a failed split keeps the reservation"
        );
    }

    #[test]
    fn split_of_an_unknown_segment_is_rejected() {
        let mut vmm = VirtualMemoryManager::new(VirtAddr::new(0x1000), 0x10000);
        vmm.mark_as_reserved(Segment::new(VirtAddr::new(0x2000), 0x4000))
            .unwrap();

        assert_eq!(
            vmm.split(
                Segment::new(VirtAddr::new(0x2000), 0x3000),
                VirtAddr::new(0x3000)
            ),
            Err(SplitSegmentError::NotReserved)
        );
    }
}
