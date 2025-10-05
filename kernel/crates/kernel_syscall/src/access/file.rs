use core::ffi::c_int;

use kernel_vfs::path::AbsolutePath;

pub trait FileInfo {}

pub trait FileAccess {
    type FileInfo: FileInfo;
    type Fd: From<c_int> + Into<c_int>;
    type OpenError;
    type ReadError;
    type WriteError;
    type CloseError;

    fn file_info(&self, path: &AbsolutePath) -> Option<Self::FileInfo>;

    fn open(&self, info: &Self::FileInfo) -> Result<Self::Fd, Self::OpenError>;

    fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, Self::ReadError>;

    fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, Self::WriteError>;

    fn close(&self, fd: Self::Fd) -> Result<(), Self::CloseError>;
}

#[cfg(test)]
pub mod testing {
    use alloc::borrow::ToOwned;
    use alloc::collections::BTreeMap;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::ffi::c_int;
    use core::sync::atomic::AtomicUsize;
    use core::sync::atomic::Ordering::Relaxed;

    use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath};
    use spin::mutex::Mutex;
    use spin::rwlock::RwLock;

    use crate::access::{FileAccess, FileInfo};

    #[derive(Default)]
    pub struct MemoryFileAccess {
        pub files: BTreeMap<AbsoluteOwnedPath, Arc<MemoryFile>>,
        open_fds: BTreeMap<MemoryFd, Arc<MemoryFile>>,
    }

    pub struct MemoryFile {
        data: RwLock<Vec<u8>>,
    }

    impl MemoryFile {
        pub fn new(data: Vec<u8>) -> Self {
            MemoryFile {
                data: RwLock::new(data),
            }
        }
    }

    #[derive(Debug, Clone)]
    pub struct MemoryFd {
        num: c_int,
        position: Arc<AtomicUsize>,
    }

    impl PartialEq for MemoryFd {
        fn eq(&self, other: &Self) -> bool {
            self.num == other.num
        }
    }

    impl Eq for MemoryFd {}

    impl PartialOrd for MemoryFd {
        fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for MemoryFd {
        fn cmp(&self, other: &Self) -> core::cmp::Ordering {
            self.num.cmp(&other.num)
        }
    }

    impl From<c_int> for MemoryFd {
        fn from(v: c_int) -> Self {
            MemoryFd {
                num: v,
                position: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl From<MemoryFd> for c_int {
        fn from(v: MemoryFd) -> Self {
            v.num
        }
    }

    pub struct MemoryFileInfo {
        path: AbsoluteOwnedPath,
    }

    impl FileInfo for MemoryFileInfo {}

    impl FileAccess for Mutex<MemoryFileAccess> {
        type FileInfo = MemoryFileInfo;
        type Fd = MemoryFd;
        type OpenError = ();
        type ReadError = ();
        type WriteError = ();
        type CloseError = ();

        fn file_info(&self, path: &AbsolutePath) -> Option<Self::FileInfo> {
            let guard = self.lock();
            if guard.files.contains_key(path) {
                Some(Self::FileInfo {
                    path: path.to_owned(),
                })
            } else {
                None
            }
        }

        fn open(&self, info: &Self::FileInfo) -> Result<Self::Fd, ()> {
            let mut guard = self.lock();

            if let Some(file) = guard.files.get(&info.path).cloned() {
                let fd_num: c_int = guard
                    .open_fds
                    .keys()
                    .fold(0, |acc, fd| if acc == fd.num { acc + 1 } else { acc });
                let fd = MemoryFd::from(fd_num);
                guard.open_fds.insert(fd.clone(), file.clone());
                Ok(fd)
            } else {
                Err(())
            }
        }

        fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, ()> {
            let guard = self.lock();

            if let Some(file) = guard.open_fds.get(&fd) {
                let data = file.data.read();
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok(len)
            } else {
                Err(())
            }
        }

        fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, ()> {
            let guard = self.lock();

            if let Some(file) = guard.open_fds.get(&fd) {
                let mut data = file.data.write();
                let file_len = data.len();
                let need_max_len = fd.position.load(Relaxed) + buf.len();
                if need_max_len > file_len {
                    data.resize(need_max_len, 0);
                }
                let _ = fd.position.fetch_add(buf.len(), Relaxed);
                Ok(buf.len())
            } else {
                Err(())
            }
        }

        fn close(&self, fd: Self::Fd) -> Result<(), ()> {
            let mut guard = self.lock();

            if guard.open_fds.remove(&fd).is_some() {
                Ok(())
            } else {
                Err(())
            }
        }
    }
}
