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
    slice[bytelen] = 0; // Null-terminate the string

    Ok(buf.addr())
}

#[instrument(level = Level::TRACE, skip(cx, buf), fields(len = buf.len()))]
pub fn sys_read<Cx: FileAccess>(cx: &Cx, fildes: Cx::Fd, buf: &mut [u8]) -> Result<usize, Errno> {
    cx.read(fildes, buf).map_err(|_| EINVAL)
}

#[instrument(level = Level::TRACE, skip(cx, buf), fields(len = buf.len()))]
pub fn sys_write<Cx: FileAccess>(cx: &Cx, fildes: Cx::Fd, buf: &[u8]) -> Result<usize, Errno> {
    cx.write(fildes, buf).map_err(|_| EINVAL)
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
/// result does not fit an offset.
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

    use kernel_abi::{EINVAL, EOVERFLOW, ERANGE, Whence};
    use kernel_vfs::path::AbsoluteOwnedPath;
    use spin::mutex::Mutex;
    use spin::rwlock::RwLock;

    use crate::access::testing::{MemoryFd, MemoryFile, MemoryFileAccess};
    use crate::access::{CwdAccess, FileAccess};
    use crate::unistd::{sys_getcwd, sys_lseek};

    #[test]
    fn test_getcwd() {
        struct Cwd<'a>(&'a RwLock<AbsoluteOwnedPath>);
        impl CwdAccess for Cwd<'_> {
            fn current_working_directory(&self) -> &RwLock<AbsoluteOwnedPath> {
                self.0
            }
        }

        for args in [
            (("/test/path", 0), Err(EINVAL)),
            (("/test/path", 10), Err(ERANGE)),
            (("/test/path", 11), Ok(())),
        ] {
            let ((path, size), expected) = args;
            let cwd = AbsoluteOwnedPath::try_from(path).unwrap().into();
            let access = Cwd(&cwd);
            let mut buf = vec![0u8; size];
            let ptr = buf.as_mut_ptr();
            let res = sys_getcwd(&access, ptr.try_into().unwrap(), buf.len());
            match expected {
                Ok(()) => match res {
                    Ok(addr) => {
                        assert_eq!(addr, ptr as usize);
                        assert_eq!(path.as_bytes(), &buf[..path.len()]);
                        assert_eq!(0, buf[path.len()]);
                    }
                    Err(e) => panic!("failed with {e} but expected success"),
                },
                Err(e) => {
                    assert_eq!(res, Err(e));
                }
            }
        }
    }

    fn lseek_fixture() -> (Mutex<MemoryFileAccess>, MemoryFd) {
        let mut file_access = MemoryFileAccess::default();
        let path = AbsoluteOwnedPath::try_from("/seek.txt").unwrap();
        file_access
            .files
            .insert(path.clone(), Arc::new(MemoryFile::new(vec![0u8; 15])));
        let cx = Mutex::new(file_access);

        let info = cx
            .file_info(path.as_ref())
            .expect("fixture file must exist");
        let fd = cx.open(&info).expect("fixture file must open");
        (cx, fd)
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
