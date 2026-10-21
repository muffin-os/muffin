use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

use kernel_device::block::BlockDevice;
use kernel_ext2::{Directory, Error, Ext2Fs, Inode, InodeAddress, RegularFile};
use kernel_vfs::fs::{FileSystem, FsHandle};
use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, Path};
use kernel_vfs::{
    CloseError, CreateMode, FsError, FsyncError, OpenError, ReadError, Stat, StatError,
    TruncateError, WriteError,
};
use spin::RwLock;

pub struct VirtualExt2Fs<T> {
    ext2fs: Ext2Fs<T>,
    handles: BTreeMap<FsHandle, Arc<OpenFile>>,
}

struct OpenFile {
    path: AbsoluteOwnedPath,
    inode: RwLock<OpenInode>,
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

        let inode = match (self.lookup(path)?, create) {
            (Some(_), CreateMode::Exclusive) => return Err(OpenError::AlreadyExists),
            (Some(found), CreateMode::Never | CreateMode::IfMissing) => {
                OpenInode::try_new(found).ok_or(OpenError::NotFound)?
            }
            (None, CreateMode::Never) => return Err(OpenError::NotFound),
            (None, CreateMode::IfMissing | CreateMode::Exclusive) => {
                OpenInode::RegularFile(self.create(path)?)
            }
        };

        let handle = FsHandle::from(FS_COUNTER.fetch_add(1, Relaxed));

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
        match &*guard {
            OpenInode::RegularFile(file) => self
                .ext2fs
                .read_from_file(file, offset, buf)
                .map_err(|_| ReadError::ReadFailed),
            OpenInode::Directory(_) => Err(ReadError::NotReadable),
        }
    }

    fn write(&mut self, handle: FsHandle, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        let inode = &self
            .handles
            .get(&handle)
            .ok_or(FsError::InvalidHandle)?
            .inode;

        let mut guard = inode.write();
        match &mut *guard {
            OpenInode::RegularFile(file) => {
                self.ext2fs
                    .write_to_file(file, offset, buf)
                    .map_err(|e| match e {
                        Error::NoSpace => WriteError::NoSpace,
                        Error::FileTooLarge => WriteError::FileTooLarge,
                        Error::InvalidSuperblock
                        | Error::UnableToReadSuperblock
                        | Error::UnableToWriteSuperblock
                        | Error::UnableToReadBlockGroupDescriptorTable
                        | Error::UnableToWriteBlockGroupDescriptorTable
                        | Error::InvalidBlockGroupDescriptor(_)
                        | Error::NotDirectory
                        | Error::NotRegularFile
                        | Error::DeviceRead
                        | Error::DeviceWrite
                        | Error::InvalidInodeAddress(_)
                        | Error::InvalidBlockIndex(_)
                        | Error::InvalidDirEntryType(_)
                        | Error::InvalidDirEntry
                        | Error::InvalidBlockAddress(_)
                        | Error::NotSupported
                        | Error::EntryExists
                        | Error::NameTooLong => WriteError::WriteFailed,
                    })
            }
            OpenInode::Directory(_) => Err(WriteError::NotWritable),
        }
    }

    fn truncate(&mut self, handle: FsHandle, len_bytes: u64) -> Result<(), TruncateError> {
        let inode = &self
            .handles
            .get(&handle)
            .ok_or(FsError::InvalidHandle)?
            .inode;
        let len_bytes = usize::try_from(len_bytes).map_err(|_| TruncateError::TruncateFailed)?;

        let mut guard = inode.write();
        match &mut *guard {
            OpenInode::RegularFile(file) => self
                .ext2fs
                .truncate_file(file, len_bytes)
                .map_err(|_| TruncateError::TruncateFailed),
            OpenInode::Directory(_) => Err(TruncateError::NotSupported),
        }
    }

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError> {
        let inode = &self
            .handles
            .get(&handle)
            .ok_or(FsError::InvalidHandle)?
            .inode;

        stat.size = inode.read().as_ref().len();
        Ok(())
    }

    // every write reaches the block device before it returns, so there is
    // nothing left to commit
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
            | Error::InvalidDirEntry
            | Error::InvalidBlockAddress(_)
            | Error::FileTooLarge
            | Error::NoSpace
            | Error::NotSupported
            | Error::EntryExists
            | Error::NameTooLong => OpenError::OpenFailed,
        })
    }

    fn create(&mut self, path: &AbsolutePath) -> Result<RegularFile, OpenError> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(OpenError::NotFound);
        };
        let mut parent = self
            .lookup(parent)?
            .and_then(|found| Directory::try_from(found).ok())
            .ok_or(OpenError::NotFound)?;
        self.create_regular_file(&mut parent, name)
            .map_err(|e| match e {
                // a parent whose direct blocks are full is out of room from the caller's view
                Error::NoSpace | Error::NotSupported => OpenError::NoSpace,
                Error::EntryExists => OpenError::AlreadyExists,
                Error::NameTooLong => OpenError::NameTooLong,
                Error::InvalidSuperblock
                | Error::UnableToReadSuperblock
                | Error::UnableToWriteSuperblock
                | Error::UnableToReadBlockGroupDescriptorTable
                | Error::UnableToWriteBlockGroupDescriptorTable
                | Error::InvalidBlockGroupDescriptor(_)
                | Error::NotDirectory
                | Error::NotRegularFile
                | Error::DeviceRead
                | Error::DeviceWrite
                | Error::InvalidInodeAddress(_)
                | Error::InvalidBlockIndex(_)
                | Error::InvalidDirEntryType(_)
                | Error::InvalidDirEntry
                | Error::InvalidBlockAddress(_)
                | Error::FileTooLarge => OpenError::OpenFailed,
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

enum OpenInode {
    RegularFile(RegularFile),
    Directory(Directory),
}

impl OpenInode {
    // None for every inode type the vfs cannot open, such as a symlink
    fn try_new(found: (InodeAddress, Inode)) -> Option<Self> {
        Some(match RegularFile::try_from(found) {
            Ok(file) => Self::RegularFile(file),
            Err(found) => Self::Directory(Directory::try_from(found).ok()?),
        })
    }
}

impl AsRef<Inode> for OpenInode {
    fn as_ref(&self) -> &Inode {
        match self {
            Self::RegularFile(f) => f,
            Self::Directory(d) => d,
        }
    }
}
