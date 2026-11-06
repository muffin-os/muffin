#![no_std]
extern crate alloc;

mod file;

use alloc::sync::Arc;
use core::ops::Deref;

pub use file::*;
use spin::RwLock;
mod fs;
mod node;

pub use fs::*;
use kernel_abi::IoctlRequest;
use kernel_vfs::fs::{FileSystem, FsHandle};
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{
    CloseError, CreateMode, FdLookup, FsError, FsyncError, IoctlError, MkdirError, MmapError,
    MmapRegion, OpenError, ReadError, Readiness, Stat, StatError, TruncateError, WriteError,
};

#[derive(Clone)]
pub struct ArcLockedDevFs {
    inner: Arc<RwLock<DevFs>>,
}

impl ArcLockedDevFs {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(DevFs::new())),
        }
    }
}

impl Default for ArcLockedDevFs {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for ArcLockedDevFs {
    type Target = RwLock<DevFs>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl FileSystem for ArcLockedDevFs {
    fn open(&mut self, path: &AbsolutePath, create: CreateMode) -> Result<FsHandle, OpenError> {
        self.inner.write().open(path, create)
    }

    fn create_directory(&mut self, path: &AbsolutePath) -> Result<(), MkdirError> {
        self.inner.write().create_directory(path)
    }

    fn close(&mut self, handle: FsHandle) -> Result<(), CloseError> {
        self.inner.write().close(handle)
    }

    fn read(
        &mut self,
        handle: FsHandle,
        buf: &mut [u8],
        offset: usize,
    ) -> Result<usize, ReadError> {
        self.inner.write().read(handle, buf, offset)
    }

    fn write(&mut self, handle: FsHandle, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        self.inner.write().write(handle, buf, offset)
    }

    fn truncate(&mut self, handle: FsHandle, len_bytes: u64) -> Result<(), TruncateError> {
        self.inner.write().truncate(handle, len_bytes)
    }

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError> {
        self.inner.write().stat(handle, stat)
    }

    fn mmap(&mut self, handle: FsHandle) -> Result<MmapRegion, MmapError> {
        self.inner.write().mmap(handle)
    }

    fn ioctl(
        &mut self,
        handle: FsHandle,
        request: IoctlRequest,
        arg: &mut [u8],
        lookup: &dyn FdLookup,
    ) -> Result<usize, IoctlError> {
        self.inner.write().ioctl(handle, request, arg, lookup)
    }

    fn fsync(&mut self, handle: FsHandle) -> Result<(), FsyncError> {
        self.inner.write().fsync(handle)
    }

    fn readiness(&mut self, handle: FsHandle) -> Result<Readiness, FsError> {
        self.inner.write().readiness(handle)
    }
}

#[cfg(test)]
mod tests {
    use kernel_vfs::{FsyncError, ReadError, StatError, WriteError};

    use super::*;

    /// This device is never readable. Thus it is different from the `ALWAYS`
    /// default of `FileSystem::readiness`.
    struct IdleDevFile;

    impl DevFile for IdleDevFile {
        fn read(&mut self, _buf: &mut [u8], _offset: usize) -> Result<usize, ReadError> {
            Err(ReadError::WouldBlock)
        }

        fn write(&mut self, _buf: &[u8], _offset: usize) -> Result<usize, WriteError> {
            Err(WriteError::NotWritable)
        }

        fn stat(&mut self, _stat: &mut Stat) -> Result<(), StatError> {
            Ok(())
        }

        fn readiness(&mut self) -> Readiness {
            Readiness::default()
        }

        fn fsync(&mut self) -> Result<(), FsyncError> {
            Ok(())
        }
    }

    /// The wrapper forwards each method by hand. When `FileSystem` gets a new
    /// method with a default body, the wrapper must not use that default.
    #[test]
    fn locked_devfs_forwards_readiness_to_the_device() {
        let path = AbsolutePath::try_new("/idle").unwrap();
        let mut fs = ArcLockedDevFs::new();
        fs.inner
            .write()
            .register_file(path, || Ok(IdleDevFile))
            .unwrap();
        let handle = fs.open(path, CreateMode::Never).unwrap();
        assert_eq!(fs.readiness(handle), Ok(Readiness::default()));
        fs.close(handle).unwrap();
        assert_eq!(fs.readiness(handle), Err(FsError::InvalidHandle));
    }
}
