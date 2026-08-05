use crate::{EINVAL, Errno};

pub const F_DUPFD: i32 = 1;
pub const F_DUPFD_CLOEXEC: i32 = 2;
pub const F_DUPFD_CLOFORK: i32 = 3;
pub const F_GETFD: i32 = 4;
pub const F_SETFD: i32 = 5;
pub const F_GETFL: i32 = 6;
pub const F_SETFL: i32 = 7;
pub const F_GETLK: i32 = 8;
pub const F_SETLK: i32 = 9;
pub const F_SETLKW: i32 = 10;
pub const F_OFD_GETLK: i32 = 11;
pub const F_OFD_SETLK: i32 = 12;
pub const F_OFD_SETLKW: i32 = 13;
pub const F_GETOWN: i32 = 14;
pub const F_GETOWN_EX: i32 = 15;
pub const F_SETOWN: i32 = 16;
pub const F_SETOWN_EX: i32 = 17;

pub const F_CLOEXEC: i32 = 1 << 1;
pub const F_CLOFORK: i32 = 1 << 2;

pub const F_RDLCK: i32 = 0;
pub const F_UNLCK: i32 = 1;
pub const F_WRLCK: i32 = 2;

pub const F_OWNER_PID: i32 = 1 << 5;
pub const F_OWNER_PGRP: i32 = 1 << 6;

pub const O_CLOEXEC: i32 = 1 << 0;
pub const O_CLOFORK: i32 = 1 << 1;
pub const O_CREAT: i32 = 1 << 2;
pub const O_DIRECTORY: i32 = 1 << 3;
pub const O_EXCL: i32 = 1 << 4;
pub const O_NOCTTY: i32 = 1 << 5;
pub const O_NOFOLLOW: i32 = 1 << 6;
pub const O_TRUNC: i32 = 1 << 7;
pub const O_TTY_INIT: i32 = 1 << 8;
pub const O_APPEND: i32 = 1 << 9;
pub const O_DSYNC: i32 = 1 << 10;
pub const O_NONBLOCK: i32 = 1 << 11;
pub const O_RSYNC: i32 = 1 << 12;
pub const O_SYNC: i32 = 1 << 13;
pub const O_ACCMODE: i32 = 1 << 14;
pub const O_EXEC: i32 = 1 << 15;
pub const O_RDONLY: i32 = 1 << 16;
pub const O_RDWR: i32 = 1 << 17;
pub const O_SEARCH: i32 = 1 << 18;
pub const O_WRONLY: i32 = 1 << 19;

pub const AT_FDCWD: i32 = 1 << 20;
pub const AT_EACCESS: i32 = 1 << 21;
pub const AT_SYMLINK_NOFOLLOW: i32 = 1 << 22;
pub const AT_SYMLINK_FOLLOW: i32 = 1 << 23;
pub const AT_REMOVEDIR: i32 = 1 << 24;

pub const POSIX_FADV_DONTNEED: i32 = 1;
pub const POSIX_FADV_NOREUSE: i32 = 2;
pub const POSIX_FADV_NORMAL: i32 = 3;
pub const POSIX_FADV_RANDOM: i32 = 4;
pub const POSIX_FADV_SEQUENTIAL: i32 = 5;
pub const POSIX_FADV_WILLNEED: i32 = 6;

/// Origin an lseek offset is measured from.
///
/// The discriminants are the POSIX `SEEK_SET`, `SEEK_CUR` and `SEEK_END`
/// numbers, which userspace passes raw in the syscall register.
#[repr(usize)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Whence {
    Set = 0,
    Cur = 1,
    End = 2,
}

impl TryFrom<usize> for Whence {
    type Error = Errno;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Set),
            1 => Ok(Self::Cur),
            2 => Ok(Self::End),
            _ => Err(EINVAL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whence_numbers_are_posix() {
        for (value, expected) in [(0, Whence::Set), (1, Whence::Cur), (2, Whence::End)] {
            assert_eq!(
                Whence::try_from(value),
                Ok(expected),
                "whence {value} should decode to the POSIX origin"
            );
        }
    }

    #[test]
    fn whence_unknown_is_einval() {
        assert_eq!(
            Whence::try_from(3),
            Err(EINVAL),
            "an origin outside 0, 1 and 2 should be rejected"
        );
    }
}
