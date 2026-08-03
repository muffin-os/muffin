use kernel_abi::IoctlRequest;
use kernel_vfs::{
    FsyncError, IoctlError, MmapError, MmapRegion, ReadError, Stat, StatError, WriteError,
};

mod block;
pub use block::*;
mod null;
pub use null::*;
mod serial;
pub use serial::*;
mod zero;
pub use zero::*;

pub trait DevFile: Send + Sync {
    fn read(&mut self, buf: &mut [u8], offset: usize) -> Result<usize, ReadError>;
    fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize, WriteError>;
    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError>;

    /// Returns a raw pointer + length for the file's backing memory.
    /// Default impl rejects with [`MmapError::NotSupported`].
    ///
    /// # Errors
    /// Returns [`MmapError::NotSupported`] when the device has no stable
    /// backing memory.
    fn mmap(&mut self) -> Result<MmapRegion, MmapError> {
        Err(MmapError::NotSupported)
    }

    /// Commits any pending writes to the underlying device.
    /// Default impl is a no-op.
    ///
    /// # Errors
    /// Returns an error when the underlying device fails to commit.
    fn fsync(&mut self) -> Result<(), FsyncError> {
        Ok(())
    }

    /// Performs a device-specific control operation.
    /// Default impl rejects with [`IoctlError::NotSupported`].
    ///
    /// # Errors
    /// Returns [`IoctlError::NotSupported`] when the device does not
    /// implement the request.
    fn ioctl(&mut self, _request: IoctlRequest, _arg: &mut [u8]) -> Result<usize, IoctlError> {
        Err(IoctlError::NotSupported)
    }
}
