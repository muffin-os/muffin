use alloc::borrow::ToOwned;
use core::ffi::c_int;
use core::slice::from_raw_parts;

use kernel_abi::{EINVAL, ENAMETOOLONG, ENOENT, Errno, PATH_MAX};
use kernel_vfs::path::{AbsolutePath, Path};
use log::debug;

use crate::access::{CwdAccess, FileAccess};
use crate::ptr::UserspacePtr;

pub fn sys_open<Cx: CwdAccess + FileAccess>(
    cx: &Cx,
    path: UserspacePtr<u8>,
    path_len: usize,
    _oflag: i32,
    _mode: i32,
) -> Result<usize, Errno> {
    if path_len > PATH_MAX {
        return Err(ENAMETOOLONG);
    }

    let path = {
        let path_bytes = unsafe { from_raw_parts(path.as_ptr(), path_len) };
        let path = core::str::from_utf8(path_bytes).map_err(|_| EINVAL)?;
        let path = Path::new(path);
        if let Ok(p) = AbsolutePath::try_new(path) {
            p.to_owned()
        } else {
            let mut p = cx.current_working_directory().read().clone();
            p.push(path);
            p
        }
    };

    debug!("path: {path:?}");

    let info = cx.file_info(path.as_ref()).ok_or(ENOENT)?;
    let fd = cx.open(&info).map_err(|_| EINVAL)?; // TODO: check error
    let fd_num = Into::<c_int>::into(fd);
    Ok(fd_num as usize)
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::sync::Arc;
    use alloc::vec;

    use kernel_abi::ENOENT;
    use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, ROOT};
    use spin::mutex::Mutex;
    use spin::rwlock::RwLock;

    use crate::UserspacePtr;
    use crate::access::testing::{MemoryFile, MemoryFileAccess};
    use crate::access::{CwdAccess, FileAccess};
    use crate::fcntl::sys_open;

    struct TestOpenCx<F> {
        cwd: RwLock<AbsoluteOwnedPath>,
        file_access: F,
    }

    impl<F> TestOpenCx<F>
    where
        F: FileAccess,
    {
        pub fn new(cwd: AbsoluteOwnedPath, file_access: F) -> Self {
            Self {
                cwd: RwLock::new(cwd),
                file_access,
            }
        }
    }

    impl<F> CwdAccess for TestOpenCx<F>
    where
        F: FileAccess,
    {
        fn current_working_directory(&self) -> &RwLock<AbsoluteOwnedPath> {
            &self.cwd
        }
    }

    impl<F> FileAccess for TestOpenCx<F>
    where
        F: FileAccess,
    {
        type FileInfo = F::FileInfo;
        type Fd = F::Fd;
        type OpenError = F::OpenError;
        type ReadError = F::ReadError;
        type WriteError = F::WriteError;
        type CloseError = F::CloseError;

        fn file_info(&self, path: &AbsolutePath) -> Option<Self::FileInfo> {
            self.file_access.file_info(path)
        }

        fn open(&self, info: &Self::FileInfo) -> Result<Self::Fd, Self::OpenError> {
            self.file_access.open(info)
        }

        fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, Self::ReadError> {
            self.file_access.read(fd, buf)
        }

        fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, Self::WriteError> {
            self.file_access.write(fd, buf)
        }

        fn close(&self, fd: Self::Fd) -> Result<(), Self::CloseError> {
            self.file_access.close(fd)
        }
    }

    #[test]
    fn test_open_not_found() {
        let file_access = MemoryFileAccess::default();
        let cx = TestOpenCx::new(ROOT.to_owned(), Mutex::new(file_access));

        let path = "/foo.txt";
        let p = UserspacePtr::try_from(path.as_ptr()).unwrap();

        let result = sys_open(&cx, p, path.len(), 0, 0);
        assert_eq!(result, Err(ENOENT));
    }

    #[test]
    fn test_open() {
        let mut file_access = MemoryFileAccess::default();
        file_access.files.insert(
            AbsoluteOwnedPath::try_from("/foo.txt").unwrap(),
            Arc::new(MemoryFile::new(vec![1_u8; 128])),
        );
        let cx = TestOpenCx::new(ROOT.to_owned(), Mutex::new(file_access));

        let path = "/foo.txt";
        let p = UserspacePtr::try_from(path.as_ptr()).unwrap();

        let result = sys_open(&cx, p, path.len(), 0, 0).expect("should be able to open file");
        assert_eq!(result, 0);
    }

    #[test]
    fn test_multiple_open_different_fd() {
        let mut file_access = MemoryFileAccess::default();
        file_access.files.insert(
            AbsoluteOwnedPath::try_from("/foo.txt").unwrap(),
            Arc::new(MemoryFile::new(vec![1_u8; 128])),
        );
        let cx = TestOpenCx::new(ROOT.to_owned(), Mutex::new(file_access));

        let path = "/foo.txt";
        let p = UserspacePtr::try_from(path.as_ptr()).unwrap();

        let result1 = sys_open(&cx, p, path.len(), 0, 0).expect("should be able to open file");
        let result2 = sys_open(&cx, p, path.len(), 0, 0).expect("should be able to open file");
        assert_eq!(
            result1,
            result2 - 1,
            "opening a file descriptor must return the lowest currently available fd number, so consecutive open calls must return consecutive fd numbers"
        );
    }
}
