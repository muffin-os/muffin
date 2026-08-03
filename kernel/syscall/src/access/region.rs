use kernel_abi::{ENODEV, Errno};

use crate::UserspacePtr;
use crate::access::{AllocationStrategy, CreateMappingError, Location};

/// Represents a tracked memory region within a process.
/// Memory regions can be accessed by kernel components like interrupt handlers.
pub trait MemoryRegion {
    /// Returns the starting address of this memory region.
    fn addr(&self) -> UserspacePtr<u8>;

    /// Returns the size in bytes of this memory region.
    fn size(&self) -> usize;
}

/// Trait for managing memory regions within a process.
/// This provides an abstraction over the process's memory region tracking.
pub trait MemoryRegionAccess {
    type Region: MemoryRegion;

    /// Creates a mapping and immediately tracks it as a memory region in the process.
    /// Returns the address of the created mapping.
    fn create_and_track_mapping(
        &self,
        location: Location,
        size: usize,
        allocation_strategy: AllocationStrategy,
    ) -> Result<UserspacePtr<u8>, CreateMappingError>;

    /// Adds a memory region to the process's memory region tracking.
    /// This makes the region available to other kernel components.
    fn add_memory_region(&self, region: Self::Region);

    /// Maps a device-file's memory into the calling process as a shared
    /// mapping, returning the address of the mapping.
    ///
    /// The default rejects with `ENODEV` because the context has no device
    /// support (POSIX: the fd type is unsupported for mmap).
    ///
    /// # Errors
    /// Returns `ENODEV` when the context does not support device-backed
    /// mappings.
    fn map_shared_file(&self, fd: i32, len: usize) -> Result<UserspacePtr<u8>, Errno> {
        let _ = (fd, len);
        Err(ENODEV)
    }
}
