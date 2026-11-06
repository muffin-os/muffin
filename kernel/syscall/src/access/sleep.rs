use kernel_abi::Errno;

/// This trait gives the sleep of a syscall that polls for a change of state.
/// The syscalls `waitpid`, `poll` and a blocking `read` use it.
pub trait SleepAccess {
    /// Sleeps for one poll interval. Returns `EINTR` when a signal interrupts the sleep.
    fn sleep_one_poll(&self) -> Result<(), Errno>;
}
