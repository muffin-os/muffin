use x86_64::VirtAddr;
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::{Page, PageSize};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct Segment {
    pub start: VirtAddr,
    pub len: u64,
}

impl Segment {
    #[must_use]
    pub const fn new(start: VirtAddr, len: u64) -> Self {
        Self { start, len }
    }

    /// The last address of the segment, inclusive.
    ///
    /// # Panics
    ///
    /// Panics if the segment is empty.
    #[must_use]
    pub fn last(&self) -> VirtAddr {
        assert!(self.len > 0);
        VirtAddr::new(self.start.as_u64() + (self.len - 1))
    }

    #[must_use]
    pub fn contains(&self, addr: VirtAddr) -> bool {
        self.len > 0 && self.start <= addr && addr <= self.last()
    }
}

impl<S: PageSize> From<&Segment> for PageRangeInclusive<S> {
    fn from(value: &Segment) -> Self {
        Self {
            start: Page::containing_address(value.start),
            end: Page::containing_address(value.last()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_at_top_of_lower_half() {
        let segment = Segment::new(VirtAddr::new(0x7FFF_FFFF_F000), 0x1000);
        assert!(segment.contains(segment.start));
        assert!(segment.contains(VirtAddr::new(0x7FFF_FFFF_FFFF)));
        assert!(!segment.contains(VirtAddr::new(0x7FFF_FFFF_EFFF)));
    }

    #[test]
    fn contains_at_top_of_address_space() {
        let segment = Segment::new(VirtAddr::new(0xFFFF_FFFF_FFFF_F000), 0x1000);
        assert!(segment.contains(segment.start));
        assert!(segment.contains(VirtAddr::new(u64::MAX)));
        assert!(!segment.contains(VirtAddr::new(0xFFFF_FFFF_FFFF_EFFF)));
    }

    #[test]
    fn contains_excludes_the_address_after_last() {
        let segment = Segment::new(VirtAddr::new(0x1000), 0x1000);
        assert!(segment.contains(VirtAddr::new(0x1FFF)));
        assert!(!segment.contains(VirtAddr::new(0x2000)));
        assert!(!Segment::new(VirtAddr::new(0x1000), 0).contains(VirtAddr::new(0x1000)));
    }

    #[test]
    fn page_range_ends_at_top_of_lower_half() {
        let segment = Segment::new(VirtAddr::new(0x7FFF_FFFF_F000), 0x1000);
        let range: PageRangeInclusive<x86_64::structures::paging::Size4KiB> = (&segment).into();
        assert_eq!(range.start, range.end);
        assert_eq!(range.end.start_address(), segment.start);
    }
}
