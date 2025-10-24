use alloc::vec::Vec;
use core::slice;

use kernel_vfs::node::VfsNode;
use spin::mutex::Mutex;
use x86_64::VirtAddr;
use x86_64::structures::paging::PhysFrame;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;

use crate::UsizeExt;
use crate::mem::virt::OwnedSegment;

pub struct MemoryRegions {
    regions: Mutex<Vec<MemoryRegion>>,
}

impl Default for MemoryRegions {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryRegions {
    pub fn new() -> Self {
        Self {
            regions: Mutex::new(Vec::new()),
        }
    }

    pub fn add_region(&self, region: MemoryRegion) {
        self.regions.lock().push(region);
    }

    pub fn with_memory_region_for_address<F, R>(&self, addr: VirtAddr, f: F) -> Option<R>
    where
        F: FnOnce(&MemoryRegion) -> R,
    {
        self.regions
            .lock()
            .iter()
            .find(|r| r.addr() <= addr && r.addr() + r.size().into_u64() > addr)
            .map(f)
    }

    pub fn is_memory_region_at_address(&self, addr: VirtAddr) -> bool {
        self.regions
            .lock()
            .iter()
            .any(|r| r.addr() <= addr && r.addr() + r.size().into_u64() > addr)
    }
}

#[derive(Debug)]
pub enum MemoryRegion {
    /// A memory region that will have its memory mapped in lazily
    /// by the page fault handler upon access to a page.
    ///
    /// - [`LazyMemoryRegion`]
    Lazy(LazyMemoryRegion),
    /// A memory region whose entire memory is already mapped.
    /// One could call it a "normal piece of memory".
    ///
    /// - [`MappedMemoryRegion`]
    Mapped(MappedMemoryRegion),
    /// A memory region that is lazy, but is additionally backed by
    /// a file. The page handler will map the pages lazily upon access,
    /// and read the bytes from the respective location from the backing
    /// file.
    ///
    /// - [`FileBackedMemoryRegion`]
    FileBacked(FileBackedMemoryRegion),
}

impl MemoryRegion {
    pub fn addr(&self) -> VirtAddr {
        match self {
            MemoryRegion::Lazy(lazy_memory_region) => lazy_memory_region.segment.start,
            MemoryRegion::Mapped(mapped_memory_region) => mapped_memory_region.segment.start,
            MemoryRegion::FileBacked(file_backed_memory_region) => {
                file_backed_memory_region.region.segment.start
            }
        }
    }

    pub fn size(&self) -> usize {
        match self {
            MemoryRegion::Lazy(lazy_memory_region) => lazy_memory_region.size,
            MemoryRegion::Mapped(mapped_memory_region) => mapped_memory_region.size,
            MemoryRegion::FileBacked(file_backed_memory_region) => {
                file_backed_memory_region.region.size
            }
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.addr().as_ptr(), self.size()) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.addr().as_mut_ptr(), self.size()) }
    }
}

#[derive(Debug)]
pub struct LazyMemoryRegion {
    segment: OwnedSegment<'static>,
    /// The size of the region. This may differ from the
    /// size of the segment in that the size of the segment
    /// is page-aligned, while this may not be.
    ///
    /// For example, the segment of a memory region whose
    /// size is 5 bytes is actually 4096 bytes.
    size: usize,
    /// The physical frames that were mapped for this lazy
    /// memory region.
    physical_frames: Mutex<Vec<PhysFrame>>,
}

impl Drop for LazyMemoryRegion {
    fn drop(&mut self) {
        todo!("deallocate physical memory")
    }
}

#[derive(Debug)]
pub struct MappedMemoryRegion {
    segment: OwnedSegment<'static>,
    size: usize,
    physical_frames: PhysFrameRangeInclusive,
}

impl MappedMemoryRegion {
    pub fn new(
        segment: OwnedSegment<'static>,
        size: usize,
        physical_frames: PhysFrameRangeInclusive,
    ) -> Self {
        Self {
            segment,
            size,
            physical_frames,
        }
    }
}

impl Drop for MappedMemoryRegion {
    fn drop(&mut self) {
        todo!("deallocate physical memory")
    }
}

#[derive(Debug)]
pub struct FileBackedMemoryRegion {
    region: LazyMemoryRegion,
    node: VfsNode,
}

impl Drop for FileBackedMemoryRegion {
    fn drop(&mut self) {
        todo!("deallocate physical memory")
    }
}
