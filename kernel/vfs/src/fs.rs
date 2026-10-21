use kernel_abi::IoctlRequest;

use crate::path::AbsolutePath;
use crate::{
    CloseError, CreateMode, FsyncError, IoctlError, MmapError, MmapRegion, OpenError, ReadError,
    Stat, StatError, TruncateError, WriteError,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct FsHandle(u64);

impl From<u64> for FsHandle {
    fn from(handle: u64) -> Self {
        FsHandle(handle)
    }
}

pub trait FileSystem: Send + Sync {
    /// Opens the file at `path`, creating it as an empty regular file first
    /// when `create` asks for it and nothing exists at `path`. Lookup and
    /// creation are one step, so no other open can slip in between.
    ///
    /// # Errors
    /// - [`OpenError::NotFound`] if nothing exists at `path` and `create` is
    ///   [`CreateMode::Never`], or if the parent directory does not exist.
    /// - [`OpenError::AlreadyExists`] if `create` is [`CreateMode::Exclusive`]
    ///   and something exists at `path`.
    /// - [`OpenError::CreateUnsupported`] if the file would have to be created
    ///   and the file system cannot create files.
    /// - [`OpenError::NoSpace`] if the file would have to be created and the
    ///   file system has no room for it.
    /// - [`OpenError::OpenFailed`] for an underlying error, such as a hardware
    ///   error.
    fn open(&mut self, path: &AbsolutePath, create: CreateMode) -> Result<FsHandle, OpenError>;

    /// # Errors
    /// Returns an error if the handle is invalid or already closed,
    /// or if there was an underlying error during closing (such as
    /// a hardware error).
    fn close(&mut self, handle: FsHandle) -> Result<(), CloseError>;

    /// Read up to `buf.len()` bytes from the file at the given
    /// `handle` into `buf` and returns the number of bytes read.
    /// The read starts at `offset`.
    ///
    /// `Ok(0)` with a non-empty `buf` means the end of the file is reached.
    ///
    /// # Errors
    ///
    /// Returns an error if the handle is invalid or already closed,
    /// or if there was an underlying error during reading (such as
    /// a hardware error).
    fn read(&mut self, handle: FsHandle, buf: &mut [u8], offset: usize)
    -> Result<usize, ReadError>;

    fn write(&mut self, handle: FsHandle, buf: &[u8], offset: usize) -> Result<usize, WriteError>;

    /// Sets the size of the file at `handle` to `len_bytes`. Bytes past
    /// `len_bytes` are discarded; growing the file appends zero bytes.
    ///
    /// # Errors
    /// - [`TruncateError::FsError`] if the handle is invalid or already closed.
    /// - [`TruncateError::NotSupported`] if the file has no adjustable size,
    ///   such as a device file.
    /// - [`TruncateError::TruncateFailed`] for an underlying error, such as a
    ///   hardware error.
    fn truncate(&mut self, handle: FsHandle, len_bytes: u64) -> Result<(), TruncateError>;

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError>;

    /// Creates and returns a memory mapping  for the given [`FsHandle`].
    ///
    /// The default impl rejects with [`MmapError::NotSupported`].
    ///
    /// # Errors
    /// Returns [`MmapError::NotSupported`] for filesystems that have no
    /// stable backing memory, or any underlying error.
    fn mmap(&mut self, _handle: FsHandle) -> Result<MmapRegion, MmapError> {
        Err(MmapError::NotSupported)
    }

    /// Performs a device-specific control operation on the file.
    ///
    /// The default impl rejects with [`IoctlError::NotSupported`].
    ///
    /// # Errors
    /// Returns [`IoctlError::NotSupported`] when the file does not implement
    /// the request.
    fn ioctl(
        &mut self,
        _handle: FsHandle,
        _request: IoctlRequest,
        _arg: &mut [u8],
    ) -> Result<usize, IoctlError> {
        Err(IoctlError::NotSupported)
    }

    /// Commits any pending writes for the given handle to the underlying
    /// device.
    ///
    /// # Errors
    /// Returns an error if the underlying device fails to commit.
    fn fsync(&mut self, _handle: FsHandle) -> Result<(), FsyncError>;
}
