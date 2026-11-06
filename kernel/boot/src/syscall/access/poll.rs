use core::time::Duration;

use kernel_abi::{EBADF, EIO, Errno, FdNum};
use kernel_syscall::access::PollAccess;
use kernel_vfs::Readiness;

use crate::hpet::hpet;
use crate::syscall::access::KernelAccess;

impl PollAccess for KernelAccess {
    type Fd = FdNum;

    fn readiness(&self, fd: Self::Fd) -> Result<Readiness, Errno> {
        let description = self
            .process
            .file_descriptors()
            .read()
            .get(&fd)
            .ok_or(EBADF)?
            .file_description()
            .clone();
        description.readiness().map_err(|_| EIO)
    }

    fn monotonic_now(&self) -> Duration {
        hpet().read().now().since_boot()
    }
}
