use crate::path::AbsolutePath;
use crate::{
    CloseError, FsyncError, MmapError, MmapRegion, OpenError, ReadError, Stat, StatError,
    WriteError,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct FsHandle(u64);

impl From<u64> for FsHandle {
    fn from(handle: u64) -> Self {
        FsHandle(handle)
    }
}

pub trait FileSystem: Send + Sync {
    /// # Errors
    /// Returns an error if the path does not point to a file, or if there
    /// was an underlying error during opening (such as a hardware error).
    fn open(&mut self, path: &AbsolutePath) -> Result<FsHandle, OpenError>;

    /// # Errors
    /// Returns an error if the handle is invalid or already closed,
    /// or if there was an underlying error during closing (such as
    /// a hardware error).
    fn close(&mut self, handle: FsHandle) -> Result<(), CloseError>;

    /// Read up to `buf.len()` bytes from the file at the given
    /// `handle` into `buf` and returns the number of bytes read.
    /// The read starts at `offset`.
    ///
    /// At the end of the file, this returns [`ReadError::EndOfFile`].
    /// **A result of `Ok(0)` does not indicate the end of the file.**
    ///
    /// # Errors
    /// Returns [`ReadError::EndOfFile`] if the end of the file is reached.
    ///
    /// Returns an error if the handle is invalid or already closed,
    /// or if there was an underlying error during reading (such as
    /// a hardware error).
    fn read(&mut self, handle: FsHandle, buf: &mut [u8], offset: usize)
    -> Result<usize, ReadError>;

    fn write(&mut self, handle: FsHandle, buf: &[u8], offset: usize) -> Result<usize, WriteError>;

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError>;

    /// Returns a raw pointer + length for the file's backing memory, suitable
    /// for direct in-kernel access (e.g. blitting pixels into a framebuffer).
    ///
    /// The default impl rejects with [`MmapError::NotSupported`].
    ///
    /// # Errors
    /// Returns [`MmapError::NotSupported`] for filesystems that have no
    /// stable backing memory, or any underlying error.
    fn mmap(&mut self, _handle: FsHandle) -> Result<MmapRegion, MmapError> {
        Err(MmapError::NotSupported)
    }

    /// Commits any pending writes for the given handle to the underlying
    /// device.
    ///
    /// # Errors
    /// Returns an error if the underlying device fails to commit.
    fn fsync(&mut self, _handle: FsHandle) -> Result<(), FsyncError>;
}
