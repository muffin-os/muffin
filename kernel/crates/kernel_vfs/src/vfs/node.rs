use alloc::sync::{Arc, Weak};
use core::fmt::{Debug, Formatter};
use core::ops::Deref;

use spin::RwLock;

use crate::fs::{FileSystem, FsHandle};
use crate::path::AbsoluteOwnedPath;
use crate::vfs::stat::Stat;
use crate::{FsError, FsyncError, MmapError, MmapRegion, ReadError, StatError, WriteError};

#[derive(Clone)]
pub struct VfsNode {
    inner: Arc<Inner>,
}

impl Debug for VfsNode {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VfsNode")
            .field("path", &self.inner.path)
            .field("fs_handle", &self.inner.fs_handle)
            .finish_non_exhaustive()
    }
}

pub struct Inner {
    path: AbsoluteOwnedPath,
    fs_handle: FsHandle,
    fs: Weak<RwLock<dyn FileSystem>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(fs) = self.fs.upgrade() {
            let mut guard = fs.write();
            let _ = guard.close(self.fs_handle);
        }
    }
}

impl Deref for VfsNode {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl VfsNode {
    pub(crate) fn new(
        path: AbsoluteOwnedPath,
        fs_handle: FsHandle,
        fs: Weak<RwLock<dyn FileSystem>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                path,
                fs_handle,
                fs,
            }),
        }
    }

    /// Reads up to `buf.len()` bytes from the file at the given
    /// `offset` into `buf` and returns the number of bytes read.
    ///
    /// See [`FileSystem::read`] for more details.
    ///
    /// # Errors
    /// Returns [`ReadError::EndOfFile`] if the end of the file is reached.
    pub fn read<B>(&self, mut buf: B, offset: usize) -> Result<usize, ReadError>
    where
        B: AsMut<[u8]>,
    {
        let fs = self.fs.upgrade().ok_or(FsError::FileSystemNotOpen)?;
        let buf = buf.as_mut();

        let mut guard = fs.write();
        guard.read(self.fs_handle, buf, offset)
    }

    pub fn write<B>(&self, buf: B, offset: usize) -> Result<usize, WriteError>
    where
        B: AsRef<[u8]>,
    {
        let fs = self.fs.upgrade().ok_or(FsError::FileSystemNotOpen)?;
        let buf = buf.as_ref();

        let mut guard = fs.write();
        guard.write(self.fs_handle, buf, offset)
    }

    pub fn stat(&self, stat: &mut Stat) -> Result<(), StatError> {
        let fs = self.fs.upgrade().ok_or(FsError::FileSystemNotOpen)?;

        let mut guard = fs.write();
        guard.stat(self.fs_handle, stat)
    }

    /// Returns a [`MmapRegion`] over this file's backing memory.
    ///
    /// # Errors
    /// Returns [`MmapError::NotSupported`] if the underlying filesystem or
    /// device file does not support `mmap`.
    pub fn mmap(&self) -> Result<MmapRegion, MmapError> {
        let fs = self.fs.upgrade().ok_or(FsError::FileSystemNotOpen)?;

        let mut guard = fs.write();
        guard.mmap(self.fs_handle)
    }

    /// Commits any pending writes for this file to the underlying device.
    ///
    /// # Errors
    /// Returns an error if the underlying device fails to commit.
    pub fn fsync(&self) -> Result<(), FsyncError> {
        let fs = self.fs.upgrade().ok_or(FsError::FileSystemNotOpen)?;

        let mut guard = fs.write();
        guard.fsync(self.fs_handle)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use crate::path::{AbsolutePath, ROOT};
    use crate::testing::TestFs;
    use crate::{CloseError, Stat, Vfs};

    #[test]
    fn test_drop() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            vec![0_u8; 1],
            Stat::default(),
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();

        let node = vfs
            .open(AbsolutePath::try_new("/foo/bar.txt").unwrap())
            .unwrap();
        let fs = node.fs.upgrade().expect("file system should still exist");

        // save the fs_handle so that we can try to close it after drop
        let fs_handle = node.fs_handle;

        drop(node);

        // closing the node's fs_handle should return an error now, because the
        // fs_handle must have been closed during drop
        assert_eq!(
            CloseError::NotOpen,
            fs.write().close(fs_handle).unwrap_err()
        );
    }

    #[test]
    fn test_no_drop() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            vec![0_u8; 1],
            Stat::default(),
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();

        let node = vfs
            .open(AbsolutePath::try_new("/foo/bar.txt").unwrap())
            .unwrap();
        let fs = node.fs.upgrade().expect("file system should still exist");

        // closing the node's fs_handle must not return an error now, because the
        // node hasn't been dropped yet
        assert!(fs.write().close(node.fs_handle).is_ok());

        drop(node);
    }
}
