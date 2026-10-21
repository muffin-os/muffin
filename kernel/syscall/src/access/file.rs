use core::ffi::c_int;

use kernel_abi::{EINVAL, ENOSYS, ENOTTY, Errno, IoctlRequest, Stat};
use kernel_vfs::CreateMode;
use kernel_vfs::path::AbsolutePath;

pub trait FileInfo {}

/// What an open file description may be used for.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum AccessMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

impl AccessMode {
    #[must_use]
    pub fn can_read(self) -> bool {
        match self {
            Self::ReadOnly | Self::ReadWrite => true,
            Self::WriteOnly => false,
        }
    }

    #[must_use]
    pub fn can_write(self) -> bool {
        match self {
            Self::WriteOnly | Self::ReadWrite => true,
            Self::ReadOnly => false,
        }
    }
}

pub trait FileAccess {
    type FileInfo: FileInfo;
    type Fd: From<c_int> + Into<c_int> + Clone + core::fmt::Debug;

    /// Resolves `path` to something `open` can turn into a descriptor,
    /// creating an empty file there first when `create` asks for it.
    ///
    /// # Errors
    /// Returns `ENOENT` when nothing exists at `path` and `create` is
    /// [`CreateMode::Never`], or when the parent directory is missing.
    /// Returns `EEXIST` when `create` is [`CreateMode::Exclusive`] and
    /// something exists at `path`, `EACCES` when the file system cannot create
    /// files, `ENOSPC` when it has no room for one, and `EIO` when the backing
    /// file system failed.
    fn file_info(&self, path: &AbsolutePath, create: CreateMode) -> Result<Self::FileInfo, Errno>;

    /// Creates a descriptor on the lowest free number. `access` limits it to
    /// reads, writes or both; with `append`, every write starts at the end of
    /// the file.
    fn open(&self, info: &Self::FileInfo, access: AccessMode, append: bool) -> Self::Fd;

    /// Reads from an open file at its current offset. `Ok(0)` on a non-empty
    /// `buf` means the end of the file.
    ///
    /// # Errors
    /// Returns `EBADF` when `fd` is not open or was opened write-only,
    /// `EISDIR` when it names a directory and `EIO` when the backing file
    /// system failed.
    fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, Errno>;

    /// Writes to an open file at its current offset, or at its end when `fd`
    /// was opened for appending.
    ///
    /// # Errors
    /// Returns `EBADF` when `fd` is not open or was opened read-only and `EIO`
    /// when the backing file system failed.
    fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, Errno>;

    /// Releases an open file descriptor.
    ///
    /// # Errors
    /// Returns `EBADF` when `fd` is not open.
    fn close(&self, fd: Self::Fd) -> Result<(), Errno>;

    /// Performs a device-specific control operation on an open file.
    ///
    /// # Errors
    /// Returns `ENOTTY` when the file does not implement the request.
    fn ioctl(&self, fd: Self::Fd, request: IoctlRequest, arg: &mut [u8]) -> Result<usize, Errno> {
        let _ = (fd, request, arg);
        Err(ENOTTY)
    }

    /// Sets the size of an open file to `len_bytes`, discarding bytes past it
    /// or appending zero bytes.
    ///
    /// # Errors
    /// Returns `EINVAL` when the file cannot be resized, such as a device
    /// file, `EBADF` when `fd` is not open and `EIO` when the backing file
    /// system failed.
    fn truncate(&self, fd: Self::Fd, len_bytes: u64) -> Result<(), Errno> {
        let _ = (fd, len_bytes);
        Err(EINVAL)
    }

    /// Flushes an open file's pending writes to its backing device.
    ///
    /// # Errors
    /// Returns `EINVAL` when the file does not support fsync.
    fn fsync(&self, fd: Self::Fd) -> Result<(), Errno> {
        let _ = fd;
        Err(EINVAL)
    }

    /// Fills file metadata for an open file.
    ///
    /// # Errors
    /// Returns `ENOSYS` when the context does not implement stat.
    fn fstat(&self, fd: Self::Fd) -> Result<Stat, Errno> {
        let _ = fd;
        Err(ENOSYS)
    }

    /// Returns the offset the next read or write on `fd` starts at.
    ///
    /// # Errors
    /// Returns `ENOSYS` when the context tracks no offset.
    fn position(&self, fd: Self::Fd) -> Result<u64, Errno> {
        let _ = fd;
        Err(ENOSYS)
    }

    /// Sets the offset the next read or write on `fd` starts at.
    ///
    /// # Errors
    /// Returns `ENOSYS` when the context tracks no offset.
    fn set_position(&self, fd: Self::Fd, position: u64) -> Result<(), Errno> {
        let _ = (fd, position);
        Err(ENOSYS)
    }
}

#[cfg(test)]
pub mod testing {
    use alloc::borrow::ToOwned;
    use alloc::collections::BTreeMap;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::ffi::c_int;

    use kernel_abi::{EBADF, EEXIST, EINVAL, ENOENT, Errno, Stat};
    use kernel_vfs::CreateMode;
    use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath};
    use spin::mutex::Mutex;
    use spin::rwlock::RwLock;

    use crate::access::{AccessMode, FileAccess, FileInfo};

    #[derive(Default)]
    pub struct MemoryFileAccess {
        pub files: BTreeMap<AbsoluteOwnedPath, Arc<MemoryFile>>,
        open_fds: BTreeMap<MemoryFd, OpenMemoryFile>,
    }

    impl MemoryFileAccess {
        fn open_fd(&mut self, fd: MemoryFd) -> Result<&mut OpenMemoryFile, Errno> {
            self.open_fds.get_mut(&fd).ok_or(EBADF)
        }
    }

    pub struct MemoryFile {
        data: RwLock<Vec<u8>>,
        is_device: bool,
    }

    impl MemoryFile {
        pub fn new(data: Vec<u8>) -> Self {
            MemoryFile {
                data: RwLock::new(data),
                is_device: false,
            }
        }

        /// A file whose size cannot change, so truncating it fails with `EINVAL`.
        pub fn device(data: Vec<u8>) -> Self {
            MemoryFile {
                data: RwLock::new(data),
                is_device: true,
            }
        }
    }

    struct OpenMemoryFile {
        file: Arc<MemoryFile>,
        position: u64,
        access: AccessMode,
        append: bool,
    }

    #[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
    pub struct MemoryFd(c_int);

    impl From<c_int> for MemoryFd {
        fn from(v: c_int) -> Self {
            MemoryFd(v)
        }
    }

    impl From<MemoryFd> for c_int {
        fn from(v: MemoryFd) -> Self {
            v.0
        }
    }

    pub struct MemoryFileInfo {
        path: AbsoluteOwnedPath,
    }

    impl FileInfo for MemoryFileInfo {}

    impl FileAccess for Mutex<MemoryFileAccess> {
        type FileInfo = MemoryFileInfo;
        type Fd = MemoryFd;

        fn file_info(
            &self,
            path: &AbsolutePath,
            create: CreateMode,
        ) -> Result<Self::FileInfo, Errno> {
            let mut guard = self.lock();
            match (guard.files.contains_key(path), create) {
                (true, CreateMode::Exclusive) => return Err(EEXIST),
                (true, CreateMode::Never | CreateMode::IfMissing) => {}
                (false, CreateMode::Never) => return Err(ENOENT),
                (false, CreateMode::IfMissing | CreateMode::Exclusive) => {
                    guard
                        .files
                        .insert(path.to_owned(), Arc::new(MemoryFile::new(Vec::new())));
                }
            }
            Ok(Self::FileInfo {
                path: path.to_owned(),
            })
        }

        fn open(&self, info: &Self::FileInfo, access: AccessMode, append: bool) -> Self::Fd {
            let mut guard = self.lock();

            let file = guard.files[&info.path].clone();
            let fd_num: c_int = guard
                .open_fds
                .keys()
                .fold(0, |acc, fd| if acc == fd.0 { acc + 1 } else { acc });
            let fd = MemoryFd(fd_num);
            guard.open_fds.insert(
                fd.clone(),
                OpenMemoryFile {
                    file,
                    position: 0,
                    access,
                    append,
                },
            );
            fd
        }

        fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, Errno> {
            let mut guard = self.lock();
            let open = guard.open_fd(fd)?;
            if !open.access.can_read() {
                return Err(EBADF);
            }

            let data = open.file.data.read();
            let position = usize::try_from(open.position)
                .unwrap_or(usize::MAX)
                .min(data.len());
            let len = (data.len() - position).min(buf.len());
            buf[..len].copy_from_slice(&data[position..position + len]);
            drop(data);
            open.position += len as u64;
            Ok(len)
        }

        fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, Errno> {
            let mut guard = self.lock();
            let open = guard.open_fd(fd)?;
            if !open.access.can_write() {
                return Err(EBADF);
            }

            let mut data = open.file.data.write();
            let position = if open.append {
                data.len()
            } else {
                usize::try_from(open.position).map_err(|_| EINVAL)?
            };
            let end = position + buf.len();
            if end > data.len() {
                data.resize(end, 0);
            }
            data[position..end].copy_from_slice(buf);
            drop(data);
            open.position = end as u64;
            Ok(buf.len())
        }

        fn close(&self, fd: Self::Fd) -> Result<(), Errno> {
            let mut guard = self.lock();

            guard.open_fds.remove(&fd).map(|_| ()).ok_or(EBADF)
        }

        fn truncate(&self, fd: Self::Fd, len_bytes: u64) -> Result<(), Errno> {
            let mut guard = self.lock();
            let open = guard.open_fd(fd)?;
            if open.file.is_device {
                return Err(EINVAL);
            }
            let len_bytes = usize::try_from(len_bytes).map_err(|_| EINVAL)?;
            open.file.data.write().resize(len_bytes, 0);
            Ok(())
        }

        fn fstat(&self, fd: Self::Fd) -> Result<Stat, Errno> {
            let mut guard = self.lock();
            let open = guard.open_fd(fd)?;
            Ok(Stat {
                size: open.file.data.read().len() as u64,
            })
        }

        fn position(&self, fd: Self::Fd) -> Result<u64, Errno> {
            let mut guard = self.lock();
            Ok(guard.open_fd(fd)?.position)
        }

        fn set_position(&self, fd: Self::Fd, position: u64) -> Result<(), Errno> {
            let mut guard = self.lock();
            guard.open_fd(fd)?.position = position;
            Ok(())
        }
    }
}
