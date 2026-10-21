use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

use kernel_device::block::BlockDevice;
use kernel_ext2::{Directory, Error, Ext2Fs, Inode, InodeAddress, RegularFile, Type};
use kernel_vfs::fs::{FileSystem, FsHandle};
use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, Path};
use kernel_vfs::{
    CloseError, CreateMode, FsError, FsyncError, OpenError, ReadError, Stat, StatError, WriteError,
};
use spin::RwLock;

pub struct VirtualExt2Fs<T> {
    ext2fs: Ext2Fs<T>,
    handles: BTreeMap<FsHandle, Arc<OpenFile>>,
}

struct OpenFile {
    path: AbsoluteOwnedPath,
    inode: RwLock<VirtualExt2Inode>,
}

impl<T> From<Ext2Fs<T>> for VirtualExt2Fs<T> {
    fn from(ext2fs: Ext2Fs<T>) -> Self {
        Self {
            ext2fs,
            handles: BTreeMap::default(),
        }
    }
}

impl<T> Deref for VirtualExt2Fs<T> {
    type Target = Ext2Fs<T>;

    fn deref(&self) -> &Self::Target {
        &self.ext2fs
    }
}

impl<T> DerefMut for VirtualExt2Fs<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ext2fs
    }
}

impl<T> FileSystem for VirtualExt2Fs<T>
where
    T: BlockDevice + Send + Sync,
{
    fn open(&mut self, path: &AbsolutePath, create: CreateMode) -> Result<FsHandle, OpenError> {
        static FS_COUNTER: AtomicU64 = AtomicU64::new(0);

        // instead of creating a new inode, check whether we already have that inode open behind another handle
        if let Some(v) = self.handles.values().find(|v| path == v.path.as_ref()) {
            if create == CreateMode::Exclusive {
                return Err(OpenError::AlreadyExists);
            }
            let handle = FsHandle::from(FS_COUNTER.fetch_add(1, Relaxed));
            self.handles.insert(handle, v.clone());
            return Ok(handle);
        }

        let (found_num, found) = match (self.lookup(path)?, create) {
            (Some(_), CreateMode::Exclusive) => return Err(OpenError::AlreadyExists),
            (Some(found), CreateMode::Never | CreateMode::IfMissing) => found,
            (None, CreateMode::Never) => return Err(OpenError::NotFound),
            (None, CreateMode::IfMissing | CreateMode::Exclusive) => {
                let parent = path.parent().ok_or(OpenError::NotFound)?;
                // shortcut: ext2 cannot create files yet, until the ext2 create path is wired in here
                return Err(match self.lookup(parent)? {
                    Some(_) => OpenError::CreateUnsupported,
                    None => OpenError::NotFound,
                });
            }
        };

        let handle = FsHandle::from(FS_COUNTER.fetch_add(1, Relaxed));
        let inode = VirtualExt2Inode::try_new(found_num, found).ok_or(OpenError::NotFound)?;

        self.handles.insert(
            handle,
            Arc::new(OpenFile {
                path: path.to_owned(),
                inode: RwLock::new(inode),
            }),
        );
        Ok(handle)
    }

    fn close(&mut self, handle: FsHandle) -> Result<(), CloseError> {
        if self.handles.remove(&handle).is_none() {
            Err(CloseError::NotOpen)
        } else {
            Ok(())
        }
    }

    fn read(
        &mut self,
        handle: FsHandle,
        buf: &mut [u8],
        offset: usize,
    ) -> Result<usize, ReadError> {
        let inode = &self
            .handles
            .get(&handle)
            .ok_or(FsError::InvalidHandle)?
            .inode;

        let guard = inode.read();
        match &guard.inner {
            Inner::RegularFile(file) => self
                .ext2fs
                .read_from_file(file, offset, buf)
                .map_err(|_| ReadError::ReadFailed),
            Inner::Directory(_) => Err(ReadError::NotReadable),
        }
    }

    fn write(
        &mut self,
        _handle: FsHandle,
        _buf: &[u8],
        _offset: usize,
    ) -> Result<usize, WriteError> {
        todo!("write support for ext2");
    }

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError> {
        let inode = &self
            .handles
            .get(&handle)
            .ok_or(FsError::InvalidHandle)?
            .inode;

        stat.size = inode.read().inner.as_ref().len();
        Ok(())
    }

    fn fsync(&mut self, _handle: FsHandle) -> Result<(), FsyncError> {
        Ok(())
    }
}

impl<T> VirtualExt2Fs<T>
where
    T: BlockDevice + Send + Sync,
{
    fn lookup(&self, path: &Path) -> Result<Option<(InodeAddress, Inode)>, OpenError> {
        self.find_inode(path).map_err(|e| match e {
            // a path component is a file, so nothing exists below it
            Error::NotDirectory => OpenError::NotFound,
            Error::InvalidSuperblock
            | Error::UnableToReadSuperblock
            | Error::UnableToWriteSuperblock
            | Error::UnableToReadBlockGroupDescriptorTable
            | Error::UnableToWriteBlockGroupDescriptorTable
            | Error::InvalidBlockGroupDescriptor(_)
            | Error::NotRegularFile
            | Error::DeviceRead
            | Error::DeviceWrite
            | Error::InvalidInodeAddress(_)
            | Error::InvalidBlockIndex(_)
            | Error::InvalidDirEntryType(_)
            | Error::NoSpace
            | Error::NotSupported
            | Error::EntryExists
            | Error::NameTooLong => OpenError::OpenFailed,
        })
    }

    fn find_inode(&self, path: &Path) -> Result<Option<(InodeAddress, Inode)>, Error> {
        let root_inode = self.ext2fs.read_root_inode()?;
        self.find_inode_from(path, root_inode.into_inner())
    }

    fn find_inode_from(
        &self,
        path: &Path,
        starting_point: (InodeAddress, Inode),
    ) -> Result<Option<(InodeAddress, Inode)>, Error> {
        let (mut current_num, mut current) = starting_point;
        for component in path.filenames() {
            // "." and ".." are ordinary ext2 directory entries, so one lookup covers them
            let Some(entry) = self
                .list_dir(&current)?
                .into_iter()
                .find(|entry| entry.name() == Some(component))
            else {
                return Ok(None);
            };
            (current_num, current) = self.resolve_dir_entry(entry)?;
        }

        Ok(Some((current_num, current)))
    }
}

pub struct VirtualExt2Inode {
    _inode_num: InodeAddress,
    inner: Inner,
}

impl VirtualExt2Inode {
    #[must_use]
    #[expect(clippy::missing_panics_doc)] // see comments
    pub fn try_new(inode_num: InodeAddress, inode: Inode) -> Option<Self> {
        let inner = match inode.typ() {
            // the unwraps don't actually panic, we check the type, try_into checks again
            Some(Type::RegularFile) => Inner::RegularFile((inode_num, inode).try_into().unwrap()),
            Some(Type::Directory) => Inner::Directory((inode_num, inode).try_into().unwrap()),
            _ => return None,
        };
        Some(Self {
            _inode_num: inode_num,
            inner,
        })
    }
}

enum Inner {
    RegularFile(RegularFile),
    Directory(Directory),
}

impl AsRef<Inode> for Inner {
    fn as_ref(&self) -> &Inode {
        match self {
            Inner::RegularFile(f) => f,
            Inner::Directory(d) => d,
        }
    }
}
