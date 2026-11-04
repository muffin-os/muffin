use core::ffi::{c_char, c_int, c_void};
use core::{mem, ptr, slice};

use kernel_abi::{
    DirentHeader, EINVAL, ENOSYS, ENOTDIR, ENOTTY, Errno, F_DUPFD, F_DUPFD_CLOEXEC, F_GETFD,
    F_GETFL, F_SETFD, F_SETFL, IoctlRequest, O_RDONLY, Stat, Whence,
};
use libc::{DIR, DT_UNKNOWN, S_IFDIR, S_IFREG, dirent, off_t, ssize_t};

use crate::errno::{fail, ret, set_errno};
use crate::mem::{free, malloc};
use crate::string::c_str;

const BLOCK_SIZE: libc::blksize_t = 4096;

/// Returns whether `fd` is a directory.
///
/// A 1-byte buffer holds no whole record, so getdents on a directory fails
/// with `EINVAL`, or returns `0` when no entry remains. It moves no position.
/// Every other file fails with another errno, such as `ENOTDIR`.
fn is_directory(fd: c_int) -> bool {
    matches!(muffin_sys::getdents(fd, &mut [0; 1]), Ok(_) | Err(EINVAL))
}

/// Ignores the `mode` argument, because the kernel has no file permissions.
///
/// # Safety
///
/// `path` must point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open(path: *const c_char, oflag: c_int, _: ...) -> c_int {
    ret(unsafe { c_str(path) }.and_then(|path| muffin_sys::open(path, oflag)))
}

/// # Safety
///
/// `buf` must be valid for `count` bytes of writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, count: usize) -> ssize_t {
    // The kernel rejects an empty buffer with EINVAL, but POSIX reads nothing.
    if count == 0 {
        return 0;
    }
    let buf = unsafe { slice::from_raw_parts_mut(buf.cast::<u8>(), count) };
    ret(muffin_sys::read(fd, buf).map(|n| n as ssize_t))
}

/// # Safety
///
/// `buf` must be valid for `count` bytes of reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: usize) -> ssize_t {
    // The kernel rejects an empty buffer with EINVAL, but POSIX writes nothing.
    if count == 0 {
        return 0;
    }
    let buf = unsafe { slice::from_raw_parts(buf.cast::<u8>(), count) };
    ret(muffin_sys::write(fd, buf).map(|n| n as ssize_t))
}

#[unsafe(no_mangle)]
pub extern "C" fn close(fd: c_int) -> c_int {
    ret(muffin_sys::close(fd).map(|()| 0))
}

#[unsafe(no_mangle)]
pub extern "C" fn lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t {
    let whence = Whence::try_from(whence);
    ret(whence
        .and_then(|whence| muffin_sys::lseek(fd, offset, whence))
        .and_then(|position| Ok(off_t::try_from(position)?)))
}

#[unsafe(no_mangle)]
pub extern "C" fn fsync(fd: c_int) -> c_int {
    ret(muffin_sys::fsync(fd).map(|()| 0))
}

fn file_status(fd: c_int) -> Result<libc::stat, Errno> {
    let mut kernel = Stat::default();
    muffin_sys::fstat(fd, &mut kernel)?;
    // SAFETY: `stat` is plain integers, so all zeros is a valid value.
    let mut status: libc::stat = unsafe { mem::zeroed() };
    status.st_size = off_t::try_from(kernel.size)?;
    // shortcut: kernel Stat has only size, so the mode is the default for the
    // file type; upgrade when kernel_abi::Stat grows mode.
    status.st_mode = if is_directory(fd) {
        S_IFDIR | 0o755
    } else {
        S_IFREG | 0o644
    };
    status.st_nlink = 1;
    status.st_blksize = BLOCK_SIZE;
    Ok(status)
}

fn path_status(path: &str) -> Result<libc::stat, Errno> {
    // The kernel opens a directory with O_RDONLY like a file, so one open
    // serves both types.
    let fd = muffin_sys::open(path, O_RDONLY)?;
    let status = file_status(fd);
    muffin_sys::close(fd)?;
    status
}

/// # Safety
///
/// `buf` must be valid for one `stat` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstat(fd: c_int, buf: *mut libc::stat) -> c_int {
    ret(file_status(fd)
        .map(|status| unsafe { buf.write(status) })
        .map(|()| 0))
}

/// # Safety
///
/// `path` must point to a NUL-terminated string, and `buf` must be valid for
/// one `stat` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stat(path: *const c_char, buf: *mut libc::stat) -> c_int {
    let status = unsafe { c_str(path) }.and_then(path_status);
    ret(status
        .map(|status| unsafe { buf.write(status) })
        .map(|()| 0))
}

/// The kernel has no symbolic links, so `lstat` equals `stat`.
///
/// # Safety
///
/// Same contract as [`stat`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lstat(path: *const c_char, buf: *mut libc::stat) -> c_int {
    unsafe { stat(path, buf) }
}

/// Accepts the descriptor and status flag commands without effect, because
/// the kernel keeps no such flags. Duplication fails with `ENOSYS`, because
/// the kernel has no dup syscall.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcntl(_: c_int, cmd: c_int, _: ...) -> c_int {
    match cmd {
        F_GETFD | F_SETFD | F_GETFL | F_SETFL => 0,
        F_DUPFD | F_DUPFD_CLOEXEC => fail(ENOSYS),
        _ => fail(EINVAL),
    }
}

/// Reads one pointer argument, the buffer of the request. An unknown request
/// fails with `ENOTTY`.
///
/// # Safety
///
/// The variadic argument must be a pointer to a buffer of the size `request`
/// needs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ioctl(fd: c_int, request: c_int, mut args: ...) -> c_int {
    let arg = unsafe { args.next_arg::<*mut c_void>() };
    let request = usize::try_from(request)
        .map_err(|_| ENOTTY)
        .and_then(IoctlRequest::try_from);
    ret(request
        .and_then(|request| unsafe { muffin_sys::ioctl_raw(fd, request, arg) })
        .map(|_| 0))
}

/// # Safety
///
/// `buf` must be valid for `size` bytes of writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getcwd(buf: *mut c_char, size: usize) -> *mut c_char {
    if buf.is_null() {
        return fail(EINVAL);
    }
    let path = unsafe { slice::from_raw_parts_mut(buf.cast::<u8>(), size) };
    ret(muffin_sys::getcwd(path).map(|()| buf))
}

/// Bytes of getdents output a directory stream buffers.
const RECORDS_SIZE: usize = 4096;

/// Open directory stream. `records` holds the getdents output from `begin`
/// to `end` that `readdir` has not returned yet.
struct Dir {
    fd: c_int,
    records: [u8; RECORDS_SIZE],
    begin: usize,
    end: usize,
    entry: dirent,
}

impl Dir {
    /// Returns the next entry, `Ok(None)` at the end of the directory.
    fn next_entry(&mut self) -> Result<Option<&mut dirent>, Errno> {
        if self.begin == self.end {
            self.end = muffin_sys::getdents(self.fd, &mut self.records)?;
            self.begin = 0;
            if self.end == 0 {
                return Ok(None);
            }
        }
        let header = DirentHeader {
            name_len: self.records[self.begin],
        };
        let name_begin = self.begin + size_of::<DirentHeader>();
        let name_end = name_begin + usize::from(header.name_len);
        let name = &self.records[name_begin..name_end];
        self.begin = name_end;

        // `name_len` is a u8, so the name and its NUL fit in `d_name`.
        for (dst, &src) in self.entry.d_name.iter_mut().zip(name) {
            *dst = src as c_char;
        }
        self.entry.d_name[name.len()] = 0;
        Ok(Some(&mut self.entry))
    }
}

/// # Safety
///
/// `path` must point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn opendir(path: *const c_char) -> *mut DIR {
    let fd = match unsafe { c_str(path) }.and_then(|path| muffin_sys::open(path, O_RDONLY)) {
        Ok(fd) => fd,
        Err(errno) => return fail(errno),
    };
    if !is_directory(fd) {
        // The ENOTDIR result wins over a close error.
        let _ = muffin_sys::close(fd);
        return fail(ENOTDIR);
    }
    let dir = malloc(size_of::<Dir>()).cast::<Dir>();
    if dir.is_null() {
        let _ = muffin_sys::close(fd);
        return dir.cast();
    }
    // SAFETY: `malloc` returned room for one `Dir`, aligned for it.
    unsafe {
        dir.write(Dir {
            fd,
            records: [0; RECORDS_SIZE],
            begin: 0,
            end: 0,
            entry: dirent {
                d_ino: 0,
                d_type: DT_UNKNOWN,
                d_name: [0; 256],
            },
        });
    }
    dir.cast()
}

/// Returns null with errno unchanged at the end of the directory. The entry
/// has `d_ino = 0` and `d_type = DT_UNKNOWN`, because getdents reports names only.
///
/// # Safety
///
/// `dir` must come from `opendir` and not be closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn readdir(dir: *mut DIR) -> *mut dirent {
    let dir = unsafe { &mut *dir.cast::<Dir>() };
    match dir.next_entry() {
        Ok(Some(entry)) => entry,
        Ok(None) => ptr::null_mut(),
        Err(errno) => {
            set_errno(errno);
            ptr::null_mut()
        }
    }
}

/// # Safety
///
/// `dir` must come from `opendir` and not be closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn closedir(dir: *mut DIR) -> c_int {
    let fd = unsafe { (*dir.cast::<Dir>()).fd };
    unsafe { free(dir.cast()) };
    close(fd)
}

/// # Safety
///
/// `dir` must come from `opendir` and not be closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dirfd(dir: *mut DIR) -> c_int {
    unsafe { (*dir.cast::<Dir>()).fd }
}

/// # Safety
///
/// `dir` must come from `opendir` and not be closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rewinddir(dir: *mut DIR) {
    let dir = unsafe { &mut *dir.cast::<Dir>() };
    // rewinddir has no error result. A failed seek surfaces in the next readdir.
    let _ = muffin_sys::lseek(dir.fd, 0, Whence::Set);
    dir.begin = 0;
    dir.end = 0;
}
