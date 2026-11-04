use core::ffi::c_int;

use crate::{EAFNOSUPPORT, EINVAL, Errno};

pub type SaFamily = u16;
pub type SockLen = u32;

pub const AF_UNIX: SaFamily = 1;
pub const SOCK_STREAM: c_int = 1;
pub const SOMAXCONN: c_int = 128;
pub const SUN_PATH_MAX: usize = 108;

/// Holds the POSIX `sockaddr_un` layout. `sun_path` holds a NUL-terminated path.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockAddrUn {
    pub sun_family: SaFamily,
    pub sun_path: [u8; SUN_PATH_MAX],
}

impl SockAddrUn {
    /// Returns `None` when `path` plus its NUL does not fit `sun_path`.
    #[must_use]
    pub fn new(path: &str) -> Option<Self> {
        if path.len() >= SUN_PATH_MAX {
            return None;
        }
        let mut sun_path = [0; SUN_PATH_MAX];
        sun_path[..path.len()].copy_from_slice(path.as_bytes());
        Some(Self {
            sun_family: AF_UNIX,
            sun_path,
        })
    }

    /// Returns the byte count the kernel reads: `sun_family`, the path, and its NUL.
    #[must_use]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> SockLen {
        let path_len = self
            .sun_path
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(SUN_PATH_MAX);
        (core::mem::offset_of!(Self, sun_path) + path_len + 1) as SockLen
    }

    /// Returns the path inside `addr`, cut at the first NUL.
    ///
    /// Returns `EINVAL` when `addr.len() < 2`, the path is empty, or the path is not UTF-8.
    /// Returns `EAFNOSUPPORT` when the family is not `AF_UNIX`.
    pub fn path_of(addr: &[u8]) -> Result<&str, Errno> {
        let (family, path) = addr.split_at_checked(size_of::<SaFamily>()).ok_or(EINVAL)?;
        if SaFamily::from_ne_bytes([family[0], family[1]]) != AF_UNIX {
            return Err(EAFNOSUPPORT);
        }
        let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
        let path = core::str::from_utf8(&path[..end]).map_err(|_| EINVAL)?;
        if path.is_empty() {
            return Err(EINVAL);
        }
        Ok(path)
    }
}

/// Tells `shutdown` which direction to close. The discriminants are the raw `SHUT_*` values.
#[repr(i32)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ShutdownHow {
    Read = 0,
    Write = 1,
    Both = 2,
}

impl TryFrom<c_int> for ShutdownHow {
    type Error = Errno;

    fn try_from(value: c_int) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Read),
            1 => Ok(Self::Write),
            2 => Ok(Self::Both),
            _ => Err(EINVAL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_of_rejects_short_addresses() {
        assert_eq!(SockAddrUn::path_of(&[1]), Err(EINVAL));
        assert_eq!(SockAddrUn::path_of(&AF_UNIX.to_ne_bytes()), Err(EINVAL));
    }

    #[test]
    fn path_of_rejects_other_families() {
        assert_eq!(
            SockAddrUn::path_of(&[2, 0, b'/', b'a', 0]),
            Err(EAFNOSUPPORT)
        );
    }

    #[test]
    fn path_of_cuts_at_the_first_nul() {
        assert_eq!(SockAddrUn::path_of(&[1, 0, b'/', b'a', 0, b'b']), Ok("/a"));
    }

    #[test]
    fn path_of_takes_a_full_path_without_nul() {
        let mut addr = [b'x'; SUN_PATH_MAX + 2];
        addr[..2].copy_from_slice(&AF_UNIX.to_ne_bytes());
        assert_eq!(SockAddrUn::path_of(&addr).map(str::len), Ok(SUN_PATH_MAX));
    }

    #[test]
    fn new_rejects_a_path_without_room_for_nul() {
        assert!(SockAddrUn::new(&"x".repeat(SUN_PATH_MAX)).is_none());
    }

    #[test]
    fn len_counts_family_path_and_nul() {
        assert_eq!(SockAddrUn::new("/a").unwrap().len(), 5);
    }

    #[test]
    fn shutdown_how_rejects_unknown_values() {
        assert_eq!(ShutdownHow::try_from(3), Err(EINVAL));
    }
}
