use alloc::sync::Arc;

use kernel_abi::{EBADF, ENOTSOCK, Errno, FdNum, ShutdownHow};
use kernel_syscall::access::SocketAccess;
use kernel_vfs::path::AbsoluteOwnedPath;

use crate::file::OpenFileDescription;
use crate::file::socket::{Socket, UNIX_SOCKETS};
use crate::syscall::access::KernelAccess;

impl KernelAccess {
    fn socket_of(&self, fd: FdNum) -> Result<Arc<Socket>, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();
        let desc = guard.get(&fd).ok_or(EBADF)?;
        desc.file_description().socket().cloned().ok_or(ENOTSOCK)
    }
}

impl SocketAccess for KernelAccess {
    type Fd = FdNum;

    fn socket(&self) -> Self::Fd {
        self.install(Arc::new(OpenFileDescription::Socket(Socket::new(
            &UNIX_SOCKETS,
        ))))
    }

    fn bind(&self, fd: Self::Fd, path: AbsoluteOwnedPath) -> Result<(), Errno> {
        self.socket_of(fd)?.bind(path)
    }

    fn listen(&self, fd: Self::Fd, backlog: usize) -> Result<(), Errno> {
        self.socket_of(fd)?.listen(backlog)
    }

    fn accept(&self, fd: Self::Fd) -> Result<Self::Fd, Errno> {
        let socket = self.socket_of(fd)?;
        let peer = self.block_on(socket.waiters(), || socket.try_accept())?;
        Ok(self.install(Arc::new(OpenFileDescription::Socket(peer))))
    }

    fn connect(&self, fd: Self::Fd, path: AbsoluteOwnedPath) -> Result<(), Errno> {
        let socket = self.socket_of(fd)?;
        self.block_on(socket.registry().waiters(), || socket.try_connect(&path))
    }

    fn shutdown(&self, fd: Self::Fd, how: ShutdownHow) -> Result<(), Errno> {
        self.socket_of(fd)?.shutdown(how)
    }
}
