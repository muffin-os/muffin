use alloc::sync::Arc;
use core::fmt::Debug;
use core::ptr::NonNull;

use kernel_abi::{Errno, FdNum};
use thiserror::Error;

use crate::FsError;

/// Memory that a file lends out for shared mappings. The bytes stay at
/// `ptr` for as long as any clone of the [`MmapRegion`] lives. User mappings
/// alias the bytes, so every access through `ptr` is `unsafe`.
#[expect(clippy::len_without_is_empty)]
pub trait MappedMemory: Debug + Send + Sync {
    fn ptr(&self) -> NonNull<u8>;
    fn len(&self) -> usize;
}

/// A share of a file's backing memory, produced by
/// [`crate::node::VfsNode::mmap`].
pub type MmapRegion = Arc<dyn MappedMemory>;

/// Resolves descriptors of the calling process during an ioctl.
pub trait FdLookup {
    /// Returns the shared memory of the file behind `fd`, as `mmap` would map it.
    ///
    /// # Errors
    /// Returns an errno when `fd` is not open or its file does not support `mmap`.
    fn mmap_region(&self, fd: FdNum) -> Result<MmapRegion, Errno>;
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum MmapError {
    #[error("{0}")]
    FsError(
        #[from]
        #[source]
        FsError,
    ),
    #[error("file does not support mmap")]
    NotSupported,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum FsyncError {
    #[error("{0}")]
    FsError(
        #[from]
        #[source]
        FsError,
    ),
    #[error("fsync failed")]
    Failed,
}
