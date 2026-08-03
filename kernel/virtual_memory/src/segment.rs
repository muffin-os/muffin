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

    #[must_use]
    pub fn contains(&self, addr: VirtAddr) -> bool {
        self.start <= addr && addr < self.start + self.len
    }
}

impl<S: PageSize> From<&Segment> for PageRangeInclusive<S> {
    fn from(value: &Segment) -> Self {
        assert!(value.len > 0);
        Self {
            start: Page::containing_address(value.start),
            end: Page::containing_address(value.start + value.len - 1),
        }
    }
}
