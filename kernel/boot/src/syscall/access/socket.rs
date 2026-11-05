use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel_abi::{EBADF, ENOTSOCK, EPIPE, Errno, FdNum, ShutdownHow};
use kernel_syscall::access::{Received, SocketAccess};
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

    fn descriptions_of(&self, fds: &[FdNum]) -> Result<Vec<Arc<OpenFileDescription>>, Errno> {
        let table = self.process.file_descriptors();
        let guard = table.read();
        fds.iter()
            .map(|fd| Ok(guard.get(fd).ok_or(EBADF)?.file_description().clone()))
            .collect()
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

    fn sendmsg(&self, fd: Self::Fd, data: &[u8], fds: &[Self::Fd]) -> Result<usize, Errno> {
        let socket = self.socket_of(fd)?;
        let items = self.descriptions_of(fds)?;
        // `block_on` retries after `EAGAIN`, so every try needs its own copy.
        match self.block_on(socket.waiters(), || socket.try_sendmsg(data, items.clone())) {
            Err(EPIPE) => Err(self.broken_pipe()),
            result => result,
        }
    }

    fn recvmsg(
        &self,
        fd: Self::Fd,
        buf: &mut [u8],
        max_fds: usize,
    ) -> Result<Received<Self::Fd>, Errno> {
        let socket = self.socket_of(fd)?;
        let (len, items) = self.block_on(socket.waiters(), || socket.try_recvmsg(buf))?;
        let truncated = items.len() > max_fds;
        let fds = items
            .into_iter()
            .take(max_fds)
            .map(|item| self.install(item))
            .collect();
        Ok(Received {
            len,
            fds,
            truncated,
        })
    }
}
