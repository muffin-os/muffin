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

/// Whether opening a path may create a regular file there.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum CreateMode {
    /// Open only what already exists.
    Never,
    /// Create an empty file when nothing exists, otherwise open the existing one.
    IfMissing,
    /// Create an empty file, failing if something already exists.
    Exclusive,
}

pub struct Vfs {
    file_systems: BTreeMap<AbsoluteOwnedPath, Fs>,
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

    /// Mounts a file system at the given mount point. Paths at or below the
    /// mount point resolve into `fs` from then on. The mount point is not
    /// looked up in the file system that contains it, so it does not have to
    /// exist there.
    ///
    /// # Errors
    /// Returns [`MountError::AlreadyMounted`] if a file system is already
    /// mounted at exactly this mount point.
    pub fn mount<P, F>(&mut self, mount_point: P, fs: F) -> Result<(), MountError>
    where
        P: AsRef<AbsolutePath>,
        F: FileSystem + 'static,
    {
        let mount_point = mount_point.as_ref().normalized();
        if self.file_systems.contains_key(&mount_point) {
            return Err(MountError::AlreadyMounted);
        }

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
        let owned = mount_point.as_ref().normalized();
        self.file_systems
            .remove(&owned)
            .map(|_| ())
            .ok_or(UnmountError::NotMounted)
    }

    /// Opens the file at the given path, creating it as [`FileSystem::open`]
    /// describes for `create`.
    ///
    /// # Errors
    /// Returns [`OpenError::NotFound`] if no file system is mounted at or
    /// above `path`, and otherwise any error of [`FileSystem::open`].
    pub fn open<P>(&self, path: P, create: CreateMode) -> Result<VfsNode, OpenError>
    where
        P: AsRef<AbsolutePath>,
    {
        let path = path.as_ref().normalized();
        let (mount_path, fs) = self.find_mount(path.as_ref()).ok_or(OpenError::NotFound)?;
        let relative_path = if mount_path == ROOT {
            path.as_ref()
        } else {
            let rest = path.strip_prefix(&***mount_path).unwrap();
            if rest.is_empty() {
                ROOT
            } else {
                AbsolutePath::try_new(rest).unwrap()
            }
        };
        let mut guard = fs.write();
        guard
            .open(relative_path, create)
            .map(|handle| VfsNode::new(path, handle, Arc::downgrade(&fs)))
    }

    fn find_mount<'a>(&'a self, path: &'a AbsolutePath) -> Option<(&'a AbsolutePath, Fs)> {
        let mut current = path;
        loop {
            if let Some(fs) = self.file_systems.get(current) {
                return Some((current, fs.clone()));
            }
            current = current.parent()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::path::{AbsolutePath, ROOT};
    use crate::testing::TestFs;
    use crate::{CreateMode, MountError, OpenError, Stat, UnmountError, Vfs};

    #[test]
    fn test_read() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            (0_u8..=u8::MAX).collect::<Vec<u8>>(),
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();

        for offset in 0..12 {
            for len in 0..14 {
                let offset = offset * 10;
                let len = len * 10;
                let node = vfs
                    .open(
                        AbsolutePath::try_new("/foo/bar.txt").unwrap(),
                        CreateMode::Never,
                    )
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
    fn test_read_at_end_of_file() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            vec![0x00; 8],
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();
        let node = vfs
            .open(
                AbsolutePath::try_new("/foo/bar.txt").unwrap(),
                CreateMode::Never,
            )
            .unwrap();

        assert_eq!(Ok(0), node.read(&mut [0_u8; 4], 8));
        assert_eq!(Ok(0), node.read(&mut [0_u8; 4], 20));
    }

    #[test]
    fn test_stat_reports_file_size() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            vec![0x00; 8],
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();
        let node = vfs
            .open(
                AbsolutePath::try_new("/foo/bar.txt").unwrap(),
                CreateMode::Never,
            )
            .unwrap();

        let mut stat = Stat::default();
        assert_eq!(Ok(()), node.stat(&mut stat));
        assert_eq!(8, stat.size);
    }

    #[test]
    fn test_mount() {
        let mut fs = TestFs::default();
        fs.insert_file(
            AbsolutePath::try_new("/foo/bar.txt").unwrap(),
            vec![0x00; 1],
        );

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();
        assert!(vfs.mount(ROOT, TestFs::default()).is_err());
    }

    #[test]
    fn test_open_mount_point_opens_fs_root() {
        let mut fs = TestFs::default();
        fs.insert_file(ROOT, vec![0x00; 1]);

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, TestFs::default()).unwrap();
        vfs.mount(AbsolutePath::try_new("/dev").unwrap(), fs)
            .unwrap();

        assert!(
            vfs.open(AbsolutePath::try_new("/dev").unwrap(), CreateMode::Never)
                .is_ok()
        );
    }

    #[test]
    fn test_open_ignores_repeated_separators() {
        let mut fs = TestFs::default();
        fs.insert_file(AbsolutePath::try_new("/x").unwrap(), vec![0x00; 1]);

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, TestFs::default()).unwrap();
        vfs.mount(AbsolutePath::try_new("/dev").unwrap(), fs)
            .unwrap();

        assert!(
            vfs.open(AbsolutePath::try_new("//dev/x").unwrap(), CreateMode::Never)
                .is_ok()
        );
        assert!(
            vfs.open(
                AbsolutePath::try_new("/dev//x/").unwrap(),
                CreateMode::Never
            )
            .is_ok()
        );
    }

    #[test]
    fn test_mount_table_ignores_repeated_separators() {
        let mut vfs = Vfs::new();
        vfs.mount(AbsolutePath::try_new("/dev").unwrap(), TestFs::default())
            .unwrap();

        assert_eq!(
            Err(MountError::AlreadyMounted),
            vfs.mount(AbsolutePath::try_new("/dev/").unwrap(), TestFs::default())
        );
        assert_eq!(
            Ok(()),
            vfs.unmount(AbsolutePath::try_new("//dev/").unwrap())
        );
    }

    #[test]
    fn test_open_strips_mount_prefix() {
        let mut root_fs = TestFs::default();
        root_fs.insert_file(AbsolutePath::try_new("/dev/x").unwrap(), vec![0xAA; 1]);
        let mut dev_fs = TestFs::default();
        dev_fs.insert_file(AbsolutePath::try_new("/x").unwrap(), vec![0xBB; 1]);

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, root_fs).unwrap();
        vfs.mount(AbsolutePath::try_new("/dev").unwrap(), dev_fs)
            .unwrap();

        let node = vfs
            .open(AbsolutePath::try_new("/dev/x").unwrap(), CreateMode::Never)
            .unwrap();
        let mut buf = [0_u8; 1];
        assert_eq!(Ok(1), node.read(&mut buf, 0));
        assert_eq!([0xBB], buf, "must resolve in the fs mounted at /dev");
        assert_eq!(
            Err(OpenError::NotFound),
            vfs.open(AbsolutePath::try_new("/x").unwrap(), CreateMode::Never)
                .map(|_| ()),
            "a mounted fs must not be reachable through the root fs"
        );
    }

    #[test]
    fn test_unmount() {
        let mut fs = TestFs::default();
        fs.insert_file(AbsolutePath::try_new("/x").unwrap(), vec![0x00; 1]);
        let dev = AbsolutePath::try_new("/dev").unwrap();

        let mut vfs = Vfs::new();
        vfs.mount(dev, fs).unwrap();
        assert!(
            vfs.open(AbsolutePath::try_new("/dev/x").unwrap(), CreateMode::Never)
                .is_ok()
        );

        assert_eq!(Ok(()), vfs.unmount(dev));
        assert_eq!(
            Err(OpenError::NotFound),
            vfs.open(AbsolutePath::try_new("/dev/x").unwrap(), CreateMode::Never)
                .map(|_| ())
        );
        assert_eq!(Err(UnmountError::NotMounted), vfs.unmount(dev));
    }

    #[test]
    fn test_open_missing_file() {
        let mut fs = TestFs::default();
        fs.insert_file(AbsolutePath::try_new("/x").unwrap(), vec![0x00; 1]);

        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();

        assert_eq!(
            Err(OpenError::NotFound),
            vfs.open(AbsolutePath::try_new("/y").unwrap(), CreateMode::Never)
                .map(|_| ())
        );
    }

    fn vfs_with_hello() -> Vfs {
        let mut fs = TestFs::default();
        fs.insert_file(AbsolutePath::try_new("/dir/hello").unwrap(), vec![0xAA; 4]);
        let mut vfs = Vfs::new();
        vfs.mount(ROOT, fs).unwrap();
        vfs
    }

    #[test]
    fn open_exclusive_on_existing_file_is_already_exists() {
        let vfs = vfs_with_hello();

        assert_eq!(
            Err(OpenError::AlreadyExists),
            vfs.open(
                AbsolutePath::try_new("/dir/hello").unwrap(),
                CreateMode::Exclusive
            )
            .map(|_| ())
        );
    }

    #[test]
    fn open_if_missing_in_missing_directory_is_not_found() {
        let vfs = vfs_with_hello();

        assert_eq!(
            Err(OpenError::NotFound),
            vfs.open(
                AbsolutePath::try_new("/nodir/new").unwrap(),
                CreateMode::IfMissing
            )
            .map(|_| ())
        );
    }

    #[test]
    fn open_if_missing_creates_an_empty_file() {
        let vfs = vfs_with_hello();
        let path = AbsolutePath::try_new("/dir/new").unwrap();

        let node = vfs.open(path, CreateMode::IfMissing).unwrap();
        let mut stat = Stat::default();
        assert_eq!(Ok(()), node.stat(&mut stat));
        assert_eq!(0, stat.size, "a created file starts empty");
        assert!(
            vfs.open(path, CreateMode::Never).is_ok(),
            "the created file must be visible to later opens"
        );
    }

    #[test]
    fn open_if_missing_on_existing_file_keeps_its_contents() {
        let vfs = vfs_with_hello();

        let node = vfs
            .open(
                AbsolutePath::try_new("/dir/hello").unwrap(),
                CreateMode::IfMissing,
            )
            .unwrap();
        let mut buf = [0_u8; 4];
        assert_eq!(Ok(4), node.read(&mut buf, 0));
        assert_eq!([0xAA; 4], buf);
    }
}
