use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

use ext2::{Ext2Fs, Inode, InodeAddress, Type};
use filesystem::BlockDevice;
use kernel_vfs::fs::{FileSystem, FsHandle};
use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, Path};
use kernel_vfs::{CloseError, FsError, OpenError, ReadError, Stat, StatError, WriteError};
use spin::RwLock;

pub struct VirtualExt2Fs<T> {
    ext2fs: Ext2Fs<T>,
    handles: BTreeMap<FsHandle, Arc<(AbsoluteOwnedPath, RwLock<VirtualExt2Inode>)>>,
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
    fn open(&mut self, path: &AbsolutePath) -> Result<FsHandle, OpenError> {
        static FS_COUNTER: AtomicU64 = AtomicU64::new(0);

        // instead of creating a new inode, check whether we already have that inode open behind another handle
        if let Some(v) = self.handles.values().find(|v| path == v.0.as_ref()) {
            let handle = FsHandle::from(FS_COUNTER.fetch_add(1, Relaxed));
            self.handles.insert(handle, v.clone());
            return Ok(handle);
        }

        let (found_num, found) = self
            .find_inode(path)
            .map_err(|_| OpenError::NotFound)?
            .ok_or(OpenError::NotFound)?;

        let handle = FsHandle::from(FS_COUNTER.fetch_add(1, Relaxed));
        let inode = VirtualExt2Inode::try_new(found_num, found).unwrap();

        self.handles
            .insert(handle, Arc::new((path.to_owned(), RwLock::new(inode))));
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
        let inode = &self.handles.get(&handle).ok_or(FsError::InvalidHandle)?.1;

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
        let inode = &self.handles.get(&handle).ok_or(FsError::InvalidHandle)?.1;

        let guard = inode.read();
        match &guard.inner {
            Inner::RegularFile(file) => {
                stat.size = file.len();
            }
            _ => todo!(),
        }
        Ok(())
    }
}

impl<T> VirtualExt2Fs<T>
where
    T: BlockDevice + Send + Sync,
{
    fn find_inode(&self, path: &Path) -> Result<Option<(InodeAddress, Inode)>, ext2::Error> {
        let root_inode = self.ext2fs.read_root_inode()?;
        self.find_inode_from(path, root_inode.into_inner())
    }

    fn find_inode_from(
        &self,
        path: &Path,
        starting_point: (InodeAddress, Inode),
    ) -> Result<Option<(InodeAddress, Inode)>, ext2::Error> {
        let components = path.filenames();

        let (mut current_num, mut current) = starting_point;
        for component in components {
            match component {
                "/" => {
                    todo!("absolute path")
                }
                "." => {} // do nothing,
                ".." => {
                    todo!("parent dir");
                }
                v => {
                    let x = current.typ();
                    if x != Type::Directory {
                        todo!("symlink support")
                    }
                    // x is a directory
                    let found_entry = self
                        .list_dir(&current)?
                        .into_iter()
                        .find(|entry| entry.name() == Some(v));
                    if let Some(found_entry) = found_entry {
                        (current_num, current) = self.resolve_dir_entry(found_entry)?;
                    } else {
                        return Ok(None);
                    }
                }
            }
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
    #[allow(clippy::missing_panics_doc)] // see comments
    pub fn try_new(inode_num: InodeAddress, inode: Inode) -> Option<Self> {
        let inner = match inode.typ() {
            // the unwraps don't actually panic, we check the type, try_into checks again
            Type::RegularFile => Inner::RegularFile((inode_num, inode).try_into().unwrap()),
            Type::Directory => Inner::Directory((inode_num, inode).try_into().unwrap()),
            _ => return None,
        };
        Some(Self {
            _inode_num: inode_num,
            inner,
        })
    }
}

enum Inner {
    RegularFile(ext2::RegularFile),
    Directory(ext2::Directory),
}

impl AsRef<Inode> for Inner {
    fn as_ref(&self) -> &Inode {
        match self {
            Inner::RegularFile(f) => f,
            Inner::Directory(d) => d,
        }
    }
}
