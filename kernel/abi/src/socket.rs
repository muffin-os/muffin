use core::ffi::c_int;
use core::mem::size_of;

use crate::{EAFNOSUPPORT, EINVAL, EMSGSIZE, Errno};

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

pub const SOL_SOCKET: i32 = 1;
pub const SCM_RIGHTS: i32 = 1;
/// `msg_flags` bit: the control buffer was too small, and the kernel closed the
/// descriptors that did not fit.
pub const MSG_CTRUNC: i32 = 8;
pub const MAX_SCM_RIGHTS: usize = 16;
pub const MAX_IOVEC: usize = 16;
pub const MAX_CONTROL_BYTES: usize = CmsgHdr::scm_rights_space(MAX_SCM_RIGHTS);

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoVec {
    pub base: usize,
    pub len: usize,
}

/// Pointers are `usize` so the kernel reads the user struct without a
/// user-space pointer type.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsgHdr {
    pub msg_name: usize,
    pub msg_namelen: u32,
    pub msg_iov: usize,
    pub msg_iovlen: usize,
    pub msg_control: usize,
    pub msg_controllen: usize,
    pub msg_flags: i32,
}

impl MsgHdr {
    #[must_use]
    pub fn new(iov: &IoVec, control: &mut [u8]) -> Self {
        Self {
            msg_name: 0,
            msg_namelen: 0,
            msg_iov: core::ptr::from_ref(iov) as usize,
            msg_iovlen: 1,
            msg_control: control.as_mut_ptr() as usize,
            msg_controllen: control.len(),
            msg_flags: 0,
        }
    }

    /// Unix stream sockets carry no name, so a name gives `EINVAL`.
    pub fn iovec_count(&self) -> Result<usize, Errno> {
        if self.msg_name != 0 || self.msg_namelen != 0 {
            return Err(EINVAL);
        }
        if self.msg_iovlen > MAX_IOVEC {
            return Err(EMSGSIZE);
        }
        Ok(self.msg_iovlen)
    }

    pub fn control_len(&self) -> Result<usize, Errno> {
        if self.msg_controllen > MAX_CONTROL_BYTES {
            return Err(EINVAL);
        }
        Ok(self.msg_controllen)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmsgHdr {
    pub cmsg_len: usize,
    pub cmsg_level: i32,
    pub cmsg_type: i32,
}

impl CmsgHdr {
    pub const FD_BYTES: usize = size_of::<c_int>();

    /// `CMSG_ALIGN(len)`: rounds `len` up to the pointer size.
    #[must_use]
    pub const fn align(len: usize) -> usize {
        (len + size_of::<usize>() - 1) & !(size_of::<usize>() - 1)
    }

    /// `CMSG_LEN(data_len)`: header plus data, no trailing padding.
    #[must_use]
    pub const fn len(data_len: usize) -> usize {
        Self::align(size_of::<CmsgHdr>()) + data_len
    }

    /// `CMSG_SPACE(data_len)`: header plus padded data.
    #[must_use]
    pub const fn space(data_len: usize) -> usize {
        Self::align(size_of::<CmsgHdr>()) + Self::align(data_len)
    }

    #[must_use]
    pub const fn scm_rights_space(fd_count: usize) -> usize {
        Self::space(fd_count * Self::FD_BYTES)
    }

    #[must_use]
    pub const fn scm_rights(fd_count: usize) -> Self {
        Self {
            cmsg_len: Self::len(fd_count * Self::FD_BYTES),
            cmsg_level: SOL_SOCKET,
            cmsg_type: SCM_RIGHTS,
        }
    }

    /// The descriptor count of an `SCM_RIGHTS` header read from a control buffer
    /// of `control_len` bytes. `None` when the level or type is different, the
    /// length leaves the buffer, or the data is not whole descriptors.
    #[must_use]
    pub fn scm_rights_count(&self, control_len: usize) -> Option<usize> {
        if self.cmsg_level != SOL_SOCKET || self.cmsg_type != SCM_RIGHTS {
            return None;
        }
        if self.cmsg_len > control_len {
            return None;
        }
        let data_len = self.cmsg_len.checked_sub(Self::len(0))?;
        data_len
            .is_multiple_of(Self::FD_BYTES)
            .then_some(data_len / Self::FD_BYTES)
    }
}

const _: () = assert!(size_of::<MsgHdr>() == 56);
const _: () = assert!(size_of::<IoVec>() == 16);
const _: () = assert!(size_of::<CmsgHdr>() == 16);

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

    #[test]
    fn cmsg_len_and_space_follow_the_posix_macros() {
        assert_eq!(CmsgHdr::len(4), 20);
        assert_eq!(CmsgHdr::space(4), 24);
        assert_eq!(CmsgHdr::space(0), 16);
    }
}
