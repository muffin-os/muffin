use core::ptr::NonNull;

use thiserror::Error;

use crate::FsError;

/// A memory region produced by [`crate::node::VfsNode::mmap`]: a raw pointer
/// to the file's backing bytes plus the length of the region.
///
/// The pointer's lifetime is tied to the underlying device or filesystem,
/// not to any Rust borrow — turning the region into a slice is the caller's
/// responsibility and is `unsafe`.
#[derive(Debug, Copy, Clone)]
pub struct MmapRegion {
    pub ptr: NonNull<u8>,
    pub len: usize,
}

unsafe impl Send for MmapRegion {}
unsafe impl Sync for MmapRegion {}

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
