use kernel_abi::PollEvents;

/// This type tells what a read or a write on an open file does now. It does
/// not do the read or the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Readiness {
    /// A read returns at once. It returns data, the end of the stream, or a
    /// pending connection.
    pub readable: bool,
    /// A write takes at least one byte at once.
    pub writable: bool,
    /// The peer is gone.
    pub hung_up: bool,
}

impl Readiness {
    /// This value applies to a regular file or a device that never blocks.
    pub const ALWAYS: Self = Self {
        readable: true,
        writable: true,
        hung_up: false,
    };

    /// Returns the `revents` of a descriptor that the caller waits on with
    /// `events`. POSIX requires `HUP` in `revents` also when the caller does not
    /// ask for it.
    #[must_use]
    pub fn revents(self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if self.readable && events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if self.writable && events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        if self.hung_up {
            revents |= PollEvents::HUP;
        }
        revents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revents_only_reports_requested_in_and_out() {
        assert_eq!(Readiness::ALWAYS.revents(PollEvents::IN), PollEvents::IN);
        assert_eq!(Readiness::ALWAYS.revents(PollEvents::OUT), PollEvents::OUT);
        assert_eq!(
            Readiness::ALWAYS.revents(PollEvents::empty()),
            PollEvents::empty()
        );
    }

    #[test]
    fn hang_up_is_reported_without_a_request() {
        let gone = Readiness {
            readable: true,
            writable: false,
            hung_up: true,
        };
        assert_eq!(
            gone.revents(PollEvents::empty()),
            PollEvents::HUP,
            "HUP must show even when the caller asked for nothing"
        );
        assert_eq!(
            gone.revents(PollEvents::IN),
            PollEvents::IN | PollEvents::HUP
        );
    }

    #[test]
    fn unknown_request_bits_are_ignored() {
        let unknown = PollEvents::from_bits_retain(0x100) | PollEvents::IN;
        assert_eq!(Readiness::ALWAYS.revents(unknown), PollEvents::IN);
    }
}
