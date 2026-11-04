use core::ffi::{c_char, c_int};

use kernel_abi::{EINVAL, ENAMETOOLONG, Errno, PATH_MAX};

use crate::errno::ret;
use crate::string::c_str;

const SHM_DIR: &str = "/dev/shm/";

/// Converts a POSIX shared memory name to its path below `/dev/shm`. The
/// name can start with one `/` and must not contain another one.
///
/// # Safety
///
/// `name` must point to a NUL-terminated string.
unsafe fn shm_path(name: *const c_char, buf: &mut [u8; PATH_MAX]) -> Result<&str, Errno> {
    let name = unsafe { c_str(name) }?;
    let name = name.strip_prefix('/').unwrap_or(name);
    if name.is_empty() || name.contains('/') {
        return Err(EINVAL);
    }
    let len = SHM_DIR.len() + name.len();
    if len > PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    buf[..SHM_DIR.len()].copy_from_slice(SHM_DIR.as_bytes());
    buf[SHM_DIR.len()..len].copy_from_slice(name.as_bytes());
    // SAFETY: both parts are UTF-8 and the join is at a character boundary.
    Ok(unsafe { core::str::from_utf8_unchecked(&buf[..len]) })
}

/// Ignores the `mode` argument, because the kernel has no file permissions.
///
/// # Safety
///
/// `name` must point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn shm_open(name: *const c_char, oflag: c_int, _: ...) -> c_int {
    let mut buf = [0; PATH_MAX];
    ret(unsafe { shm_path(name, &mut buf) }.and_then(|path| muffin_sys::open(path, oflag)))
}

/// # Safety
///
/// `name` must point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn shm_unlink(name: *const c_char) -> c_int {
    let mut buf = [0; PATH_MAX];
    ret(unsafe { shm_path(name, &mut buf) }
        .and_then(muffin_sys::unlink)
        .map(|()| 0))
}
