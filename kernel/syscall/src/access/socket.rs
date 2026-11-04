use core::ffi::c_int;

use kernel_abi::{Errno, ShutdownHow};
use kernel_vfs::path::AbsoluteOwnedPath;

pub trait SocketAccess {
    type Fd: From<c_int> + Into<c_int> + Clone + core::fmt::Debug;

    /// Creates an unbound AF_UNIX stream socket on the lowest free descriptor.
    fn socket(&self) -> Self::Fd;
    fn bind(&self, fd: Self::Fd, path: AbsoluteOwnedPath) -> Result<(), Errno>;
    fn listen(&self, fd: Self::Fd, backlog: usize) -> Result<(), Errno>;
    /// Blocks until a connection is pending.
    fn accept(&self, fd: Self::Fd) -> Result<Self::Fd, Errno>;
    /// Blocks while the listener's backlog is full.
    fn connect(&self, fd: Self::Fd, path: AbsoluteOwnedPath) -> Result<(), Errno>;
    fn shutdown(&self, fd: Self::Fd, how: ShutdownHow) -> Result<(), Errno>;
}
