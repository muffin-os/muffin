use core::slice::from_raw_parts_mut;

use kernel_abi::{EINVAL, EOVERFLOW, ERANGE, Errno, IoctlRequest, Whence};
use tracing::{Level, instrument};

use crate::access::{CwdAccess, FileAccess};
use crate::ptr::UserspaceMutPtr;

#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_getcwd<Cx: CwdAccess>(
    cx: &Cx,
    buf: UserspaceMutPtr<u8>,
    size: usize,
) -> Result<usize, Errno> {
    if buf.as_ptr().is_null() {
        return Err(EINVAL);
    }
    if size == 0 {
        return Err(EINVAL);
    }

    let mut buf = buf;
    let slice = unsafe { from_raw_parts_mut(buf.as_mut_ptr(), size) };

    let cwd = cx.current_working_directory();
    let guard = cwd.read();
    let bytelen = guard.len();
    if size <= bytelen {
        return Err(ERANGE);
    }
    slice.iter_mut().zip(guard.bytes()).for_each(|(s, b)| {
        *s = b;
    });
    slice[bytelen] = 0;

    Ok(buf.addr())
}

#[instrument(level = Level::TRACE, skip(cx, buf), fields(len = buf.len()))]
pub fn sys_read<Cx: FileAccess>(cx: &Cx, fildes: Cx::Fd, buf: &mut [u8]) -> Result<usize, Errno> {
    cx.read(fildes, buf)
}

#[instrument(level = Level::TRACE, skip(cx, buf), fields(len = buf.len()))]
pub fn sys_write<Cx: FileAccess>(cx: &Cx, fildes: Cx::Fd, buf: &[u8]) -> Result<usize, Errno> {
    cx.write(fildes, buf)
}

pub fn sys_ioctl<Cx: FileAccess>(
    cx: &Cx,
    fildes: Cx::Fd,
    request: IoctlRequest,
    arg: &mut [u8],
) -> Result<usize, Errno> {
    cx.ioctl(fildes, request, arg)
}

pub fn sys_fsync<Cx: FileAccess>(cx: &Cx, fildes: Cx::Fd) -> Result<usize, Errno> {
    cx.fsync(fildes).map(|()| 0)
}

/// Moves an open file's offset and returns the new absolute offset.
///
/// An offset beyond the end of the file is legal and is not clamped. A read
/// there returns nothing and a write extends the file.
///
/// # Errors
/// `EINVAL` for a result before the start of the file. `EOVERFLOW` when the
/// result does not fit an offset. Any error of [`FileAccess::position`],
/// [`FileAccess::fstat`] or [`FileAccess::set_position`], such as `EBADF` or
/// `ENOSYS`, is passed through.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_lseek<Cx: FileAccess>(
    cx: &Cx,
    fildes: Cx::Fd,
    offset: i64,
    whence: Whence,
) -> Result<usize, Errno> {
    let base = match whence {
        Whence::Set => 0,
        Whence::Cur => cx.position(fildes.clone())?,
        Whence::End => cx.fstat(fildes.clone())?.size,
    };
    let target =
        base.checked_add_signed(offset)
            .ok_or(if offset < 0 { EINVAL } else { EOVERFLOW })?;
    cx.set_position(fildes, target)?;
    usize::try_from(target).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec;

    use kernel_abi::{EBADF, EINVAL, EOVERFLOW, ERANGE, Whence};
    use kernel_vfs::CreateMode;
    use kernel_vfs::path::AbsoluteOwnedPath;
    use spin::mutex::Mutex;
    use spin::rwlock::RwLock;

    use crate::access::testing::{MemoryFd, MemoryFile, MemoryFileAccess};
    use crate::access::{AccessMode, CwdAccess, FileAccess};
    use crate::unistd::{sys_getcwd, sys_lseek, sys_read, sys_write};

    struct Cwd(RwLock<AbsoluteOwnedPath>);

    impl Cwd {
        fn new(path: &str) -> Self {
            Self(AbsoluteOwnedPath::try_from(path).unwrap().into())
        }
    }

    impl CwdAccess for Cwd {
        fn current_working_directory(&self) -> &RwLock<AbsoluteOwnedPath> {
            &self.0
        }
    }

    #[test]
    fn getcwd_rejects_zero_size() {
        let cwd = Cwd::new("/test/path");
        let mut buf = [0u8; 1];
        let ptr = buf.as_mut_ptr().try_into().unwrap();

        assert_eq!(Err(EINVAL), sys_getcwd(&cwd, ptr, 0));
    }

    #[test]
    fn getcwd_needs_room_for_the_nul() {
        let cwd = Cwd::new("/test/path");
        let mut buf = [0u8; 10];
        let ptr = buf.as_mut_ptr().try_into().unwrap();

        assert_eq!(Err(ERANGE), sys_getcwd(&cwd, ptr, buf.len()));
    }

    #[test]
    fn getcwd_writes_nul_terminated_path() {
        let cwd = Cwd::new("/test/path");
        let mut buf = [0xFFu8; 11];
        let ptr = buf.as_mut_ptr();

        let returned = sys_getcwd(&cwd, ptr.try_into().unwrap(), buf.len()).unwrap();

        assert_eq!(ptr as usize, returned);
        assert_eq!(b"/test/path\0", &buf);
    }

    fn lseek_fixture() -> (Mutex<MemoryFileAccess>, MemoryFd) {
        let mut file_access = MemoryFileAccess::default();
        let path = AbsoluteOwnedPath::try_from("/seek.txt").unwrap();
        file_access
            .files
            .insert(path.clone(), Arc::new(MemoryFile::new(vec![0u8; 15])));
        let cx = Mutex::new(file_access);

        let info = cx
            .file_info(path.as_ref(), CreateMode::Never)
            .expect("fixture file must exist");
        let fd = cx.open(&info, AccessMode::ReadWrite, false);
        (cx, fd)
    }

    #[test]
    fn sys_read_and_write_reject_unopened_fd() {
        let (cx, _fd) = lseek_fixture();
        let unopened = MemoryFd::from(99);

        assert_eq!(Err(EBADF), sys_read(&cx, unopened.clone(), &mut [0_u8; 4]));
        assert_eq!(Err(EBADF), sys_write(&cx, unopened, b"data"));
    }

    #[test]
    fn sys_read_at_end_of_file_returns_zero() {
        let (cx, fd) = lseek_fixture();
        cx.set_position(fd.clone(), 15)
            .expect("fixture file must accept a position");

        assert_eq!(Ok(0), sys_read(&cx, fd, &mut [0_u8; 4]));
    }

    #[test]
    fn sys_write_data_reads_back_from_the_same_position() {
        let (cx, fd) = lseek_fixture();

        assert_eq!(Ok(3), sys_write(&cx, fd.clone(), b"abc"));
        assert_eq!(Ok(3), cx.position(fd.clone()));

        cx.set_position(fd.clone(), 0)
            .expect("fixture file must accept a position");
        let mut buf = [0_u8; 3];
        assert_eq!(Ok(3), sys_read(&cx, fd, &mut buf));
        assert_eq!(b"abc", &buf);
    }

    #[test]
    fn sys_lseek_computes_offsets_from_whence() {
        let (cx, fd) = lseek_fixture();

        let result = sys_lseek(&cx, fd.clone(), 7, Whence::Set);
        assert_eq!(
            result,
            Ok(7),
            "SEEK_SET should land exactly on the requested offset"
        );
        assert_eq!(
            cx.position(fd.clone()),
            Ok(7),
            "SEEK_SET must update the tracked position"
        );

        let result = sys_lseek(&cx, fd.clone(), 3, Whence::Cur);
        assert_eq!(
            result,
            Ok(10),
            "SEEK_CUR should add the offset to the current position"
        );

        let result = sys_lseek(&cx, fd.clone(), 0, Whence::End);
        assert_eq!(
            result,
            Ok(15),
            "SEEK_END with a zero offset should land on the file size"
        );

        let result = sys_lseek(&cx, fd.clone(), -1, Whence::End);
        assert_eq!(
            result,
            Ok(14),
            "SEEK_END should accept a negative offset that stays before the end"
        );

        let result = sys_lseek(&cx, fd.clone(), 100, Whence::Set);
        assert_eq!(
            result,
            Ok(100),
            "seeking past the end of the file is legal POSIX behavior"
        );
    }

    #[test]
    fn sys_lseek_rejects_invalid_results() {
        let (cx, fd) = lseek_fixture();

        let before = cx
            .position(fd.clone())
            .expect("fixture file must report a position");
        let result = sys_lseek(&cx, fd.clone(), -1, Whence::Set);
        assert_eq!(
            result,
            Err(EINVAL),
            "SEEK_SET before the start of the file must be rejected"
        );
        assert_eq!(
            cx.position(fd.clone()),
            Ok(before),
            "a rejected seek must not move the position"
        );

        let result = sys_lseek(&cx, fd.clone(), i64::MIN, Whence::End);
        assert_eq!(
            result,
            Err(EINVAL),
            "SEEK_END far enough negative to underflow must be rejected"
        );

        // Only a base at the top of the offset space can overflow. The file size
        // plus any offset stays far below it.
        cx.set_position(fd.clone(), u64::MAX)
            .expect("fixture file must accept a position");
        let result = sys_lseek(&cx, fd.clone(), 1, Whence::Cur);
        assert_eq!(
            result,
            Err(EOVERFLOW),
            "a SEEK_CUR result past the largest representable offset must be rejected"
        );
    }
}
