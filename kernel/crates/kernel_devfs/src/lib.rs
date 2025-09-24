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
use kernel_vfs::fs::{FileSystem, FsHandle};
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{CloseError, OpenError, ReadError, Stat, StatError, WriteError};

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
    fn open(&mut self, path: &AbsolutePath) -> Result<FsHandle, OpenError> {
        self.inner.write().open(path)
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

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError> {
        self.inner.write().stat(handle, stat)
    }
}
