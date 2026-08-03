use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

pub use error::*;
use spin::RwLock;

use crate::fs::FileSystem;
use crate::node::VfsNode;
use crate::path::{AbsoluteOwnedPath, AbsolutePath, ROOT};

mod error;
mod mmap;
pub mod node;
mod stat;
pub use mmap::*;
pub use stat::*;

#[cfg(test)]
pub mod testing;

type Fs = Arc<RwLock<dyn FileSystem>>;

pub struct Vfs {
    file_systems: BTreeMap<AbsoluteOwnedPath, Fs>, // TODO: maybe a trie would be better here?
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfs {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            file_systems: BTreeMap::new(),
        }
    }

    /// Mounts a file system at the given mount point.
    /// The mount point must point to an empty directory.
    ///
    /// # Errors
    /// This function returns an error if the mount point is already mounted,
    /// not an empty directory or if another error occurs during mounting.
    pub fn mount<P, F>(&mut self, mount_point: P, fs: F) -> Result<(), MountError>
    where
        P: AsRef<AbsolutePath>,
        F: FileSystem + 'static,
    {
        let mount_point = mount_point.as_ref();
        let mount_point = mount_point.to_owned();
        if self.file_systems.contains_key(&mount_point) {
            return Err(MountError::AlreadyMounted);
        }

        // TODO: check whether the mount_point is a directory

        self.file_systems
            .insert(mount_point, Arc::new(RwLock::new(fs)));
        Ok(())
    }

    /// Unmounts the file system at the given mount point.
    ///
    /// # Errors
    /// This function returns an error if the mount point is not mounted,
    /// or if another error occurs during unmounting.
    pub fn unmount<P>(&mut self, mount_point: P) -> Result<(), UnmountError>
    where
        P: AsRef<AbsolutePath>,
    {
        let owned = mount_point.as_ref().to_owned();
        self.file_systems
            .remove(&owned)
            .map(|_| ())
            .ok_or(UnmountError::NotMounted)
    }

    /// Opens a file at the given path.
    ///
    /// # Errors
    /// This function returns an error if the file does not exist,
    /// or if another error occurs during opening.
    pub fn open<P>(&self, path: P) -> Result<VfsNode, OpenError>
    where
        P: AsRef<AbsolutePath>,
    {
        // FIXME: reuse already open VfsNodes

        let path = path.as_ref();
        let (mount_path, fs) = self.find_mount(path).ok_or(OpenError::NotFound)?;
        let relative_path = if mount_path == ROOT {
            path
        } else {
            path.strip_prefix(&***mount_path).unwrap()
        };
        let relative_path = unsafe { AbsolutePath::new_unchecked((&relative_path).as_ref()) };
        let mut guard = fs.write();
        guard
            .open(relative_path)
            .map(|handle| VfsNode::new(path.to_owned(), handle, Arc::downgrade(&fs)))
    }

    fn find_mount<'a>(&'a self, path: &'a AbsolutePath) -> Option<(&'a AbsolutePath, Fs)> {
        let mut current = path;
        if let Some(fs) = self.file_systems.get(current) {
            return Some((path, fs.clone()));
        }
        while let Some(parent) = current.parent() {
            if let Some(fs) = self.file_systems.get(parent) {
                return Some((parent, fs.clone()));
            }
            current = parent;
        }
        self.file_systems.get(ROOT).cloned().map(|v| (ROOT, v))
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::path::{AbsolutePath, ROOT};
    use crate::testing::TestFs;
    use crate::{Stat, Vfs};

    #[test]
    fn test_read() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            (0_u8..=u8::MAX).collect::<Vec<u8>>(),
            Stat::default(),
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();

        for offset in 0..12 {
            for len in 0..14 {
                let offset = offset * 10;
                let len = len * 10;
                let node = vfs
                    .open(AbsolutePath::try_new("/foo/bar.txt").unwrap())
                    .unwrap();
                let mut buf = vec![0_u8; len];
                let bytes_read = node.read(&mut buf, offset).unwrap();
                assert_eq!(bytes_read, len, "offset: {}, len: {}", offset, len);
                assert_eq!(
                    buf,
                    (offset as u8..offset as u8 + len as u8).collect::<Vec<u8>>(),
                    "offset: {}, len: {}",
                    offset,
                    len,
                );
            }
        }
    }

    #[test]
    fn test_mount() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            vec![0x00; 1],
            Stat::default(),
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();
        assert!(vfs.mount(ROOT, TestFs::default()).is_err());
    }
}
