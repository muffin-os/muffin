use core::ffi::c_int;
use core::fmt::{Debug, Display};
use core::num::TryFromIntError;

#[derive(Copy, Clone, Eq, PartialEq)]
#[repr(transparent)]
pub struct Errno(c_int);

impl Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", errno_name(self))
    }
}

impl Debug for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}({})", self, self.0)
    }
}

impl From<Errno> for c_int {
    fn from(value: Errno) -> Self {
        value.0
    }
}

impl From<Errno> for isize {
    fn from(errno: Errno) -> Self {
        errno.0 as isize
    }
}

impl From<isize> for Errno {
    fn from(errno: isize) -> Self {
        Errno(errno as c_int)
    }
}

impl From<TryFromIntError> for Errno {
    fn from(_: TryFromIntError) -> Self {
        EOVERFLOW
    }
}

macro_rules! n {
    ($($name:ident = $val:expr),*,) => {
        $(pub const $name: Errno = Errno($val);)*

        #[allow(dead_code)]
        #[must_use] pub fn errno_name(n: &Errno) -> &'static str {
            match n.0 {
                $( $val => stringify!($name), )*
                _ => "<unknown>",
            }
        }
    };
}

n! {
    E2BIG = 1,
    EACCES = 2,
    EADDRINUSE = 3,
    EADDRNOTAVAIL = 4,
    EAFNOSUPPORT = 5,
    EAGAIN = 6,
    EALREADY = 7,
    EBADF = 8,
    EBADMSG = 9,
    EBUSY = 10,
    ECANCELED = 11,
    ECHILD = 12,
    ECONNABORTED = 13,
    ECONNREFUSED = 14,
    EDEADLK = 15,
    EDESTADDRREQ = 16,
    EDOM = 17,
    EDQUOT = 18,
    EEXIST = 19,
    EFAULT = 20,
    EFBIG = 21,
    EHOSTUNREACH = 22,
    EIDRM = 23,
    EILSEQ = 24,
    EINPROGRESS = 25,
    EINTR = 26,
    EINVAL = 27,
    EIO = 28,
    EISCONN = 29,
    EISDIR = 30,
    ELOOP = 31,
    EMFILE = 32,
    EMLINK = 33,
    EMSGSIZE = 34,
    EMULTIHOP = 35,
    ENAMETOOLONG = 36,
    ENETDOWN = 37,
    ENETRESET = 38,
    ENETUNREACH = 39,
    ENFILE = 40,
    ENOBUFS = 41,
    ENODEV = 42,
    ENOENT = 43,
    ENOEXEC = 44,
    ENOLCK = 45,
    ENOLINK = 46,
    ENOMEM = 47,
    ENOMSG = 48,
    ENOPROTOOPT = 49,
    ENOSPC = 50,
    ENOSYS = 51,
    ENOTCONN = 52,
    ENOTDIR = 53,
    ENOTEMPTY = 54,
    ENOTRECOVERABLE = 55,
    ENOTSOCK = 56,
    ENOTSUP = 57,
    ENOTTY = 58,
    ENXIO = 59,
    EOPNOTSUPP = 60,
    EOVERFLOW = 61,
    EOWNERDEAD = 62,
    EPERM = 63,
    EPIPE = 64,
    EPROTO = 65,
    EPROTONOSUPPORT = 66,
    EPROTOTYPE = 67,
    ERANGE = 68,
    EROFS = 69,
    ESOCKTNOSUPPORT = 70,
    ESPIPE = 71,
    ESRCH = 72,
    ESTALE = 73,
    ETIMEDEOUT = 74,
    ETXTBSY = 75,
    EWOULDBLOCK = 76,
    EXDEV = 77,
}
