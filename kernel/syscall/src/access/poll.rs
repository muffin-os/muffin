use core::ffi::c_int;
use core::time::Duration;

use kernel_abi::{Errno, PollFd};
use kernel_vfs::Readiness;

/// This trait gives `poll` the readiness of descriptors, a clock, and a wait
/// for readiness changes.
pub trait PollAccess {
    type Fd: From<c_int> + Into<c_int> + Clone + core::fmt::Debug;

    /// Returns the [`Readiness`] of `fd`. Returns `EBADF` when `fd` is not open.
    fn readiness(&self, fd: Self::Fd) -> Result<Readiness, Errno>;
    /// Returns the time since boot. A poll with a timeout uses it for its deadline.
    fn monotonic_now(&self) -> Duration;
    /// Parks the caller until an entry of `fds` can have nonzero `revents`,
    /// a descriptor changes, the `deadline` since boot passes, or a signal
    /// arrives. A return does not promise readiness. The caller checks again.
    ///
    /// # Errors
    /// Returns `EINTR` when a signal interrupts the wait. Returns each error
    /// of [`PollAccess::readiness`] other than `EBADF`.
    fn wait_for_readiness(&self, fds: &[PollFd], deadline: Option<Duration>) -> Result<(), Errno>;
}
