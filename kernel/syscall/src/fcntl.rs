use alloc::borrow::ToOwned;
use core::ffi::c_int;
use core::slice::from_raw_parts;

use kernel_abi::{EINVAL, ENAMETOOLONG, Errno, OpenFlags, PATH_MAX};
use kernel_vfs::CreateMode;
use kernel_vfs::path::{AbsolutePath, Path};
use tracing::{Level, debug, instrument};

use crate::access::{AccessMode, CwdAccess, FileAccess};
use crate::ptr::UserspacePtr;

/// Opens the file at `path`, relative to the working directory unless
/// absolute, and returns the new descriptor number.
///
/// `oflag` must hold exactly one of `O_RDONLY`, `O_WRONLY` and `O_RDWR`.
/// `O_CREAT` creates an empty file if none exists, `O_CREAT | O_EXCL` fails if
/// one does, `O_TRUNC` empties the file and `O_APPEND` makes every write start
/// at the end. `mode` is ignored, because the kernel has no file permissions.
///
/// # Errors
/// `ENAMETOOLONG` for a path longer than `PATH_MAX`. `EINVAL` for a path that
/// is not UTF-8, an unknown bit in `oflag`, no or several access modes,
/// `O_TRUNC` with `O_RDONLY`, or `O_EXCL` without `O_CREAT`. Any error of
/// [`FileAccess::file_info`] or [`FileAccess::truncate`] is passed through;
/// a failed truncate leaves no descriptor behind.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_open<Cx: CwdAccess + FileAccess>(
    cx: &Cx,
    path: UserspacePtr<u8>,
    path_len: usize,
    oflag: i32,
    mode: i32,
) -> Result<usize, Errno> {
    let flags = OpenFlags::from_bits(oflag).ok_or(EINVAL)?;
    let access = access_mode(flags)?;
    let truncate = flags.contains(OpenFlags::TRUNC);
    if truncate && access == AccessMode::ReadOnly {
        return Err(EINVAL);
    }
    let create = match (
        flags.contains(OpenFlags::CREAT),
        flags.contains(OpenFlags::EXCL),
    ) {
        (true, true) => CreateMode::Exclusive,
        (true, false) => CreateMode::IfMissing,
        (false, true) => return Err(EINVAL),
        (false, false) => CreateMode::Never,
    };

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

    debug!(?path, "open");

    let info = cx.file_info(path.as_ref(), create)?;
    let fd = cx.open(&info, access, flags.contains(OpenFlags::APPEND));
    if truncate && let Err(errno) = cx.truncate(fd.clone(), 0) {
        cx.close(fd)?;
        return Err(errno);
    }
    let fd_num = Into::<c_int>::into(fd);
    Ok(fd_num as usize)
}

fn access_mode(flags: OpenFlags) -> Result<AccessMode, Errno> {
    match (
        flags.contains(OpenFlags::RDONLY),
        flags.contains(OpenFlags::WRONLY),
        flags.contains(OpenFlags::RDWR),
    ) {
        (true, false, false) => Ok(AccessMode::ReadOnly),
        (false, true, false) => Ok(AccessMode::WriteOnly),
        (false, false, true) => Ok(AccessMode::ReadWrite),
        _ => Err(EINVAL),
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::sync::Arc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ffi::c_int;

    use kernel_abi::{
        EBADF, EEXIST, EINVAL, ENOENT, Errno, O_APPEND, O_CREAT, O_EXCL, O_RDONLY, O_RDWR, O_TRUNC,
        O_WRONLY, Stat, Whence,
    };
    use kernel_vfs::CreateMode;
    use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, ROOT};
    use spin::mutex::Mutex;
    use spin::rwlock::RwLock;

    use crate::UserspacePtr;
    use crate::access::testing::{MemoryFd, MemoryFile, MemoryFileAccess};
    use crate::access::{AccessMode, CwdAccess, FileAccess};
    use crate::fcntl::sys_open;
    use crate::unistd::{sys_lseek, sys_read, sys_write};

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

        fn file_info(
            &self,
            path: &AbsolutePath,
            create: CreateMode,
        ) -> Result<Self::FileInfo, Errno> {
            self.file_access.file_info(path, create)
        }

        fn open(&self, info: &Self::FileInfo, access: AccessMode, append: bool) -> Self::Fd {
            self.file_access.open(info, access, append)
        }

        fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, Errno> {
            self.file_access.read(fd, buf)
        }

        fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, Errno> {
            self.file_access.write(fd, buf)
        }

        fn close(&self, fd: Self::Fd) -> Result<(), Errno> {
            self.file_access.close(fd)
        }

        fn truncate(&self, fd: Self::Fd, len_bytes: u64) -> Result<(), Errno> {
            self.file_access.truncate(fd, len_bytes)
        }

        fn fstat(&self, fd: Self::Fd) -> Result<Stat, Errno> {
            self.file_access.fstat(fd)
        }

        fn position(&self, fd: Self::Fd) -> Result<u64, Errno> {
            self.file_access.position(fd)
        }

        fn set_position(&self, fd: Self::Fd, position: u64) -> Result<(), Errno> {
            self.file_access.set_position(fd, position)
        }
    }

    type MemoryCx = TestOpenCx<Mutex<MemoryFileAccess>>;

    fn cx_with(files: Vec<(&str, MemoryFile)>) -> MemoryCx {
        let mut file_access = MemoryFileAccess::default();
        for (path, file) in files {
            file_access
                .files
                .insert(AbsoluteOwnedPath::try_from(path).unwrap(), Arc::new(file));
        }
        TestOpenCx::new(ROOT.to_owned(), Mutex::new(file_access))
    }

    fn open(cx: &MemoryCx, path: &str, oflag: c_int) -> Result<usize, Errno> {
        let p = UserspacePtr::try_from(path.as_ptr()).unwrap();
        sys_open(cx, p, path.len(), oflag, 0)
    }

    fn fd(num: usize) -> MemoryFd {
        MemoryFd::from(c_int::try_from(num).unwrap())
    }

    fn size(cx: &MemoryCx, num: usize) -> u64 {
        cx.fstat(fd(num)).expect("fd must be open").size
    }

    #[test]
    fn open_rejects_unknown_flag_bits() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);

        assert_eq!(Err(EINVAL), open(&cx, "/foo.txt", O_RDONLY | 1 << 30));
    }

    #[test]
    fn write_only_fd_rejects_read() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);
        let num = open(&cx, "/foo.txt", O_WRONLY).unwrap();

        assert_eq!(Err(EBADF), sys_read(&cx, fd(num), &mut [0_u8; 4]));
        assert_eq!(Ok(4), sys_write(&cx, fd(num), b"data"));
    }

    #[test]
    fn read_only_fd_rejects_write() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);
        let num = open(&cx, "/foo.txt", O_RDONLY).unwrap();

        assert_eq!(Err(EBADF), sys_write(&cx, fd(num), b"data"));
        assert_eq!(
            8,
            size(&cx, num),
            "a rejected write must not touch the file"
        );
        assert_eq!(Ok(4), sys_read(&cx, fd(num), &mut [0_u8; 4]));
    }

    #[test]
    fn read_write_fd_allows_both() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);
        let num = open(&cx, "/foo.txt", O_RDWR).unwrap();

        assert_eq!(Ok(4), sys_write(&cx, fd(num), b"data"));
        assert_eq!(Ok(4), sys_read(&cx, fd(num), &mut [0_u8; 4]));
    }

    #[test]
    fn append_writes_land_at_the_end_after_a_seek() {
        let cx = cx_with(vec![("/log.txt", MemoryFile::new(b"abc".to_vec()))]);
        let num = open(&cx, "/log.txt", O_WRONLY | O_APPEND).unwrap();

        assert_eq!(Ok(0), sys_lseek(&cx, fd(num), 0, Whence::Set));
        assert_eq!(Ok(2), sys_write(&cx, fd(num), b"de"));
        assert_eq!(
            Ok(5),
            cx.position(fd(num)),
            "position follows the appended bytes"
        );

        let reader = open(&cx, "/log.txt", O_RDONLY).unwrap();
        let mut buf = [0_u8; 8];
        assert_eq!(Ok(5), sys_read(&cx, fd(reader), &mut buf));
        assert_eq!(b"abcde", &buf[..5]);
    }

    #[test]
    fn open_requires_exactly_one_access_mode() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);

        for oflag in [
            0,
            O_CREAT,
            O_RDONLY | O_WRONLY,
            O_RDONLY | O_RDWR,
            O_WRONLY | O_RDWR,
            O_RDONLY | O_WRONLY | O_RDWR,
        ] {
            assert_eq!(
                Err(EINVAL),
                open(&cx, "/foo.txt", oflag),
                "oflag {oflag:#x}"
            );
        }
    }

    #[test]
    fn open_rejects_truncate_on_read_only() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);

        assert_eq!(Err(EINVAL), open(&cx, "/foo.txt", O_RDONLY | O_TRUNC));
        assert_eq!(Ok(0), open(&cx, "/foo.txt", O_RDONLY));
        assert_eq!(
            8,
            size(&cx, 0),
            "a rejected O_TRUNC must not touch the file"
        );
    }

    #[test]
    fn open_rejects_exclusive_without_create() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);

        assert_eq!(Err(EINVAL), open(&cx, "/foo.txt", O_RDWR | O_EXCL));
    }

    #[test]
    fn open_create_on_missing_file_creates_an_empty_file() {
        let cx = cx_with(vec![]);

        let num = open(&cx, "/new.txt", O_CREAT | O_RDWR).expect("O_CREAT must create");
        assert_eq!(0, size(&cx, num));
        assert_eq!(Ok(0), sys_read(&cx, fd(num), &mut [0_u8; 4]));
        assert!(
            open(&cx, "/new.txt", O_RDONLY).is_ok(),
            "the created file must be visible to later opens"
        );
    }

    #[test]
    fn open_create_on_existing_file_keeps_its_contents() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);

        let num = open(&cx, "/foo.txt", O_CREAT | O_RDONLY).expect("O_CREAT must open");
        assert_eq!(8, size(&cx, num));
    }

    #[test]
    fn open_create_exclusive_on_existing_file_is_eexist() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);

        assert_eq!(
            Err(EEXIST),
            open(&cx, "/foo.txt", O_CREAT | O_EXCL | O_WRONLY)
        );
    }

    #[test]
    fn open_truncate_empties_the_file() {
        let cx = cx_with(vec![("/foo.txt", MemoryFile::new(vec![1; 8]))]);

        let num = open(&cx, "/foo.txt", O_WRONLY | O_TRUNC).expect("O_TRUNC must open");
        assert_eq!(0, size(&cx, num));
    }

    #[test]
    fn open_truncate_failure_closes_the_fd() {
        let cx = cx_with(vec![("/dev.txt", MemoryFile::device(vec![1; 8]))]);

        assert_eq!(Err(EINVAL), open(&cx, "/dev.txt", O_WRONLY | O_TRUNC));
        assert_eq!(
            Ok(0),
            open(&cx, "/dev.txt", O_WRONLY),
            "the fd opened before the failed truncate must be free again"
        );
    }

    #[test]
    fn test_open_not_found() {
        let file_access = MemoryFileAccess::default();
        let cx = TestOpenCx::new(ROOT.to_owned(), Mutex::new(file_access));

        let path = "/foo.txt";
        let p = UserspacePtr::try_from(path.as_ptr()).unwrap();

        let result = sys_open(&cx, p, path.len(), O_RDONLY, 0);
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

        let result =
            sys_open(&cx, p, path.len(), O_RDONLY, 0).expect("should be able to open file");
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

        let result1 =
            sys_open(&cx, p, path.len(), O_RDONLY, 0).expect("should be able to open file");
        let result2 =
            sys_open(&cx, p, path.len(), O_RDONLY, 0).expect("should be able to open file");
        assert_eq!(
            result1,
            result2 - 1,
            "opening a file descriptor must return the lowest currently available fd number, so consecutive open calls must return consecutive fd numbers"
        );
    }
}
