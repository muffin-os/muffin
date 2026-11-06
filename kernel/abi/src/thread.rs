//! This module defines the ABI of the thread syscalls. A thread is one task of
//! a process. The kernel identifies each thread with a [`ThreadId`] that is
//! unique in the system.

use core::fmt::{Display, Formatter};

use bitflags::bitflags;

/// Before a call, the x86-64 SysV ABI aligns the stack to this number of bytes.
pub const THREAD_STACK_ALIGN: usize = 16;
/// A call reserves this number of bytes for its return address.
/// At thread entry, the stack pointer has this remainder modulo [`THREAD_STACK_ALIGN`].
/// The kernel pushes no return address.
pub const THREAD_ENTRY_STACK_REMAINDER: usize = size_of::<u64>();

/// This type identifies one thread. The id of a thread never repeats while the
/// system runs.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ThreadId(u64);

impl ThreadId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl Display for ThreadId {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

bitflags! {
    /// This argument holds the flags of `SYS_THREAD_EXIT`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ThreadExitFlags: usize {
        /// No thread joins the exiting thread. The kernel keeps no exit
        /// record, so `SYS_THREAD_JOIN` on this thread fails with `ESRCH`.
        const DETACHED = 1;
    }
}
