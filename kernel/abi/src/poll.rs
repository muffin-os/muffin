use bitflags::bitflags;

/// With this `timeout` argument, poll waits until a descriptor is ready.
pub const POLL_TIMEOUT_INFINITE: isize = -1;

bitflags! {
    /// These are the bits of `events` and `revents` in [`PollFd`]. Userspace can
    /// set bits that the kernel does not know. The kernel keeps these bits in
    /// `events` and ignores them.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PollEvents: i16 {
        /// A read returns at once.
        const IN = 0x1;
        /// A write takes at least one byte at once.
        const OUT = 0x4;
        /// The peer is gone. Only `revents` contains this bit.
        const HUP = 0x10;
        /// The descriptor is not open. Only `revents` contains this bit.
        const NVAL = 0x20;
    }
}

/// This is the POSIX `struct pollfd` from `<poll.h>`. The kernel writes `revents`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PollFd {
    /// The kernel skips a negative descriptor and sets its `revents` to zero.
    pub fd: i32,
    pub events: PollEvents,
    pub revents: PollEvents,
}

impl PollFd {
    #[must_use]
    pub const fn new(fd: i32, events: PollEvents) -> Self {
        Self {
            fd,
            events,
            revents: PollEvents::empty(),
        }
    }
}

const _: () = assert!(size_of::<PollEvents>() == size_of::<i16>());
