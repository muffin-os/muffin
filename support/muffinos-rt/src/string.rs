use core::ffi::{CStr, c_char, c_int};
use core::slice;

use kernel_abi::{EINVAL, ERANGE, Errno, errno_name};

/// Borrows the NUL-terminated string at `raw` as UTF-8. Fails with `EINVAL`,
/// the errno the kernel returns for a path or argument that is not UTF-8.
///
/// # Safety
///
/// `raw` must point to a NUL-terminated string that outlives `'a`.
pub(crate) unsafe fn c_str<'a>(raw: *const c_char) -> Result<&'a str, Errno> {
    unsafe { CStr::from_ptr(raw) }.to_str().map_err(|_| EINVAL)
}

/// Copies `src` into `dst` as a NUL-terminated string, truncated to
/// `dst.len() - 1` bytes. Returns `false` when `src` does not fit.
fn copy_c_string(src: &[u8], dst: &mut [u8]) -> bool {
    let Some(room) = dst.len().checked_sub(1) else {
        return false;
    };
    let copied = src.len().min(room);
    dst[..copied].copy_from_slice(&src[..copied]);
    dst[copied] = 0;
    copied == src.len()
}

/// XSI `strerror_r`: writes the errno name, such as `ENOENT`, and returns `0`,
/// or `ERANGE` when `len` cannot hold it.
///
/// # Safety
///
/// `buf` must be valid for `len` bytes of writes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strerror_r(errnum: c_int, buf: *mut c_char, len: usize) -> c_int {
    if len == 0 {
        return ERANGE.into();
    }
    let name = errno_name(&Errno::from(errnum as isize));
    let dst = unsafe { slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
    if copy_c_string(name.as_bytes(), dst) {
        0
    } else {
        ERANGE.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_fits_with_nul() {
        let mut dst = [0xff; 7];
        assert!(
            copy_c_string(b"ENOENT", &mut dst),
            "6 bytes and NUL fit in 7"
        );
        assert_eq!(&dst, b"ENOENT\0");
    }

    #[test]
    fn copy_truncates_to_len_minus_one() {
        let mut dst = [0xff; 4];
        assert!(
            !copy_c_string(b"ENOENT", &mut dst),
            "truncation is reported"
        );
        assert_eq!(&dst, b"ENO\0", "the copy keeps len - 1 bytes and a NUL");
    }

    #[test]
    fn copy_into_empty_buffer_fails() {
        assert!(!copy_c_string(b"E", &mut []), "no room for the NUL");
    }
}
