use core::ffi::c_int;

use kernel_abi::{ENODEV, Errno, ProtFlags};

use crate::UserspacePtr;
use crate::access::{AllocationStrategy, CreateMappingError, Location};

/// A memory region tracked by the process, visible to other kernel
/// components such as interrupt handlers.
pub trait MemoryRegion {
    fn addr(&self) -> UserspacePtr<u8>;

    /// Size in bytes.
    fn size(&self) -> usize;
}

pub trait MemoryRegionAccess {
    type Region: MemoryRegion;
    type Fd: From<c_int> + core::fmt::Debug;

    /// Creates a mapping and immediately tracks it as a memory region in the process.
    /// Returns the address of the created mapping.
    fn create_and_track_mapping(
        &self,
        location: Location,
        size: usize,
        allocation_strategy: AllocationStrategy,
        prot: ProtFlags,
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
    fn map_shared_file(
        &self,
        fd: Self::Fd,
        len: usize,
        prot: ProtFlags,
    ) -> Result<UserspacePtr<u8>, Errno> {
        let _ = (fd, len, prot);
        Err(ENODEV)
    }

    /// Maps `len` bytes of the file behind `fd`, starting at `offset`, into the
    /// calling process as a private lazily paged mapping, returning its address.
    ///
    /// The default rejects with `ENODEV` because the context has no filesystem.
    /// POSIX uses that errno for an fd whose type mmap does not support.
    ///
    /// # Errors
    /// Returns `ENODEV` when the context does not support file-backed mappings.
    fn map_private_file(
        &self,
        fd: Self::Fd,
        offset: usize,
        len: usize,
        prot: ProtFlags,
    ) -> Result<UserspacePtr<u8>, Errno> {
        let _ = (fd, offset, len, prot);
        Err(ENODEV)
    }
}
