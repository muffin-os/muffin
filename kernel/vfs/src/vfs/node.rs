use alloc::sync::{Arc, Weak};
use core::fmt::{Debug, Formatter};
use core::ops::Deref;

use kernel_abi::IoctlRequest;
use spin::RwLock;

use crate::fs::{FileSystem, FsHandle};
use crate::path::AbsoluteOwnedPath;
use crate::vfs::stat::Stat;
use crate::{
    FsError, FsyncError, IoctlError, MmapError, MmapRegion, ReadError, StatError, TruncateError,
    WriteError,
};

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
    /// See [`FileSystem::read`] for details, including how the end of the file is signalled.
    ///
    /// # Errors
    /// Returns an error if the file system is no longer mounted or if the read fails.
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

    /// Sets the size of this file to `len_bytes`, as [`FileSystem::truncate`]
    /// describes.
    ///
    /// # Errors
    /// Returns [`FsError::FileSystemNotOpen`] if the file system is no longer
    /// mounted, and otherwise any error of [`FileSystem::truncate`].
    pub fn truncate(&self, len_bytes: u64) -> Result<(), TruncateError> {
        let fs = self.fs.upgrade().ok_or(FsError::FileSystemNotOpen)?;

        let mut guard = fs.write();
        guard.truncate(self.fs_handle, len_bytes)
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

    /// Performs a device-specific control operation on this file.
    ///
    /// # Errors
    /// Returns [`IoctlError::NotSupported`] if the underlying filesystem or
    /// device file does not support the request.
    pub fn ioctl(&self, request: IoctlRequest, arg: &mut [u8]) -> Result<usize, IoctlError> {
        let fs = self.fs.upgrade().ok_or(FsError::FileSystemNotOpen)?;

        let mut guard = fs.write();
        guard.ioctl(self.fs_handle, request, arg)
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
    use alloc::vec::Vec;

    use crate::node::VfsNode;
    use crate::path::{AbsolutePath, ROOT};
    use crate::testing::TestFs;
    use crate::{CloseError, CreateMode, FsError, Stat, TruncateError, Vfs};

    #[test]
    fn test_drop() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            vec![0_u8; 1],
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();

        let node = vfs
            .open(
                AbsolutePath::try_new("/foo/bar.txt").unwrap(),
                CreateMode::Never,
            )
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
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();

        let node = vfs
            .open(
                AbsolutePath::try_new("/foo/bar.txt").unwrap(),
                CreateMode::Never,
            )
            .unwrap();
        let fs = node.fs.upgrade().expect("file system should still exist");

        // closing the node's fs_handle must not return an error now, because the
        // node hasn't been dropped yet
        assert!(fs.write().close(node.fs_handle).is_ok());

        drop(node);
    }

    fn open_file_with(data: &[u8]) -> (Vfs, VfsNode) {
        let mut fs = TestFs::default();
        fs.insert_file(AbsolutePath::try_new("/f").unwrap(), data.to_vec());
        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();
        let node = vfs
            .open(AbsolutePath::try_new("/f").unwrap(), CreateMode::Never)
            .unwrap();
        (vfs, node)
    }

    fn contents(node: &VfsNode) -> Vec<u8> {
        let mut stat = Stat::default();
        node.stat(&mut stat).unwrap();
        let mut buf = vec![0xFF_u8; stat.size];
        assert_eq!(Ok(stat.size), node.read(&mut buf, 0));
        buf
    }

    #[test]
    fn truncate_shrinks_the_file() {
        let (_vfs, node) = open_file_with(b"abcdef");

        assert_eq!(Ok(()), node.truncate(2));
        assert_eq!(b"ab".to_vec(), contents(&node));

        assert_eq!(Ok(()), node.truncate(0));
        assert_eq!(Vec::<u8>::new(), contents(&node));
    }

    #[test]
    fn truncate_grows_the_file_with_zeros() {
        let (_vfs, node) = open_file_with(b"ab");

        assert_eq!(Ok(()), node.truncate(5));
        assert_eq!(b"ab\0\0\0".to_vec(), contents(&node));
    }

    #[test]
    fn truncate_after_unmount_is_fs_error() {
        let (mut vfs, node) = open_file_with(b"ab");
        vfs.unmount(ROOT).unwrap();

        assert_eq!(
            Err(TruncateError::FsError(FsError::FileSystemNotOpen)),
            node.truncate(0)
        );
    }
}
