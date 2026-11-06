use core::ffi::c_int;
use core::time::Duration;

use kernel_abi::Errno;
use kernel_vfs::Readiness;

/// This trait gives `poll` the readiness of descriptors and a clock.
pub trait PollAccess {
    type Fd: From<c_int> + Into<c_int> + Clone + core::fmt::Debug;

    /// Returns the [`Readiness`] of `fd`. Returns `EBADF` when `fd` is not open.
    fn readiness(&self, fd: Self::Fd) -> Result<Readiness, Errno>;
    /// Returns the time since boot. A poll with a timeout uses it for its deadline.
    fn monotonic_now(&self) -> Duration;
}
