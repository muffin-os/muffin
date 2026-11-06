use core::time::Duration;

use kernel_abi::{EBADF, Errno, PollEvents, PollFd};
use tracing::{Level, instrument};

use crate::access::{PollAccess, SleepAccess};

/// Runs POSIX poll. Writes the `revents` of each entry and returns the number
/// of entries with nonzero `revents`. With a `timeout` of `None`, it waits
/// until an entry is ready. With `Some(Duration::ZERO)`, it returns at once.
/// It returns `0` when the timeout passes and no entry is ready.
///
/// It skips an entry with a negative `fd`. An entry with a `fd` that is not
/// open gets `NVAL` and counts as ready.
///
/// # Errors
/// Returns `EINTR` when a signal interrupts the wait. Returns each error of
/// [`PollAccess::readiness`] other than `EBADF`.
#[instrument(level = Level::TRACE, skip(cx, fds), fields(nfds = fds.len()))]
pub fn sys_poll<Cx: PollAccess + SleepAccess>(
    cx: &Cx,
    fds: &mut [PollFd],
    timeout: Option<Duration>,
) -> Result<usize, Errno> {
    // A deadline after the end of the clock never comes. Thus poll waits as
    // for `None`.
    let deadline = timeout.and_then(|t| cx.monotonic_now().checked_add(t));
    // shortcut: busy poll on `sleep_one_poll` instead of a parked waiter
    loop {
        let ready = fill_revents(cx, fds)?;
        if ready > 0 {
            return Ok(ready);
        }
        if deadline.is_some_and(|d| cx.monotonic_now() >= d) {
            return Ok(0);
        }
        cx.sleep_one_poll()?;
    }
}

fn fill_revents<Cx: PollAccess>(cx: &Cx, fds: &mut [PollFd]) -> Result<usize, Errno> {
    let mut ready = 0;
    for entry in fds.iter_mut() {
        entry.revents = PollEvents::empty();
        if entry.fd < 0 {
            continue;
        }
        let revents = match cx.readiness(Cx::Fd::from(entry.fd)) {
            Ok(readiness) => readiness.revents(entry.events),
            Err(EBADF) => PollEvents::NVAL,
            Err(e) => return Err(e),
        };
        entry.revents = revents;
        if !revents.is_empty() {
            ready += 1;
        }
    }
    Ok(ready)
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use core::cell::{Cell, RefCell};
    use core::ffi::c_int;

    use kernel_abi::EINTR;
    use kernel_vfs::Readiness;

    use super::*;

    const IDLE: Readiness = Readiness {
        readable: false,
        writable: false,
        hung_up: false,
    };
    const READABLE: Readiness = Readiness {
        readable: true,
        ..IDLE
    };

    #[derive(Default)]
    struct TestContext {
        readiness: RefCell<BTreeMap<c_int, Readiness>>,
        now: Cell<Duration>,
        sleeps: Cell<u32>,
        should_interrupt_sleep: bool,
        /// After this many sleeps, `fd` gets the readiness.
        becomes_ready: Option<(u32, c_int, Readiness)>,
    }

    impl TestContext {
        fn with(fds: &[(c_int, Readiness)]) -> Self {
            let cx = Self::default();
            cx.readiness.borrow_mut().extend(fds.iter().copied());
            cx
        }
    }

    impl PollAccess for TestContext {
        type Fd = c_int;

        fn readiness(&self, fd: c_int) -> Result<Readiness, Errno> {
            self.readiness.borrow().get(&fd).copied().ok_or(EBADF)
        }

        fn monotonic_now(&self) -> Duration {
            self.now.get()
        }
    }

    impl SleepAccess for TestContext {
        fn sleep_one_poll(&self) -> Result<(), Errno> {
            if self.should_interrupt_sleep {
                return Err(EINTR);
            }
            self.now.set(self.now.get() + Duration::from_millis(1));
            self.sleeps.set(self.sleeps.get() + 1);
            if let Some((after, fd, readiness)) = self.becomes_ready
                && self.sleeps.get() == after
            {
                self.readiness.borrow_mut().insert(fd, readiness);
            }
            Ok(())
        }
    }

    fn entry(fd: c_int, events: PollEvents) -> PollFd {
        PollFd::new(fd, events)
    }

    #[test]
    fn ready_at_once_returns_the_count_and_fills_revents() {
        let cx = TestContext::with(&[(3, READABLE), (4, IDLE), (5, Readiness::ALWAYS)]);
        let mut fds = [
            entry(3, PollEvents::IN),
            entry(4, PollEvents::IN | PollEvents::OUT),
            entry(5, PollEvents::OUT),
        ];
        assert_eq!(sys_poll(&cx, &mut fds, None), Ok(2));
        assert_eq!(fds[0].revents, PollEvents::IN);
        assert_eq!(fds[1].revents, PollEvents::empty());
        assert_eq!(fds[2].revents, PollEvents::OUT);
        assert_eq!(cx.sleeps.get(), 0);
    }

    #[test]
    fn zero_timeout_with_nothing_ready_returns_zero_without_sleeping() {
        let cx = TestContext::with(&[(3, IDLE)]);
        let mut fds = [entry(3, PollEvents::IN)];
        fds[0].revents = PollEvents::IN;
        assert_eq!(sys_poll(&cx, &mut fds, Some(Duration::ZERO)), Ok(0));
        assert_eq!(
            fds[0].revents,
            PollEvents::empty(),
            "stale revents are cleared"
        );
        assert_eq!(cx.sleeps.get(), 0);
    }

    #[test]
    fn deadline_is_honoured() {
        let cx = TestContext::with(&[(3, IDLE)]);
        let mut fds = [entry(3, PollEvents::IN)];
        assert_eq!(
            sys_poll(&cx, &mut fds, Some(Duration::from_millis(5))),
            Ok(0)
        );
        assert_eq!(cx.sleeps.get(), 5);
    }

    #[test]
    fn waits_until_an_entry_becomes_ready() {
        let cx = TestContext {
            becomes_ready: Some((3, 4, READABLE)),
            ..TestContext::with(&[(4, IDLE)])
        };
        let mut fds = [entry(4, PollEvents::IN)];
        assert_eq!(sys_poll(&cx, &mut fds, None), Ok(1));
        assert_eq!(fds[0].revents, PollEvents::IN);
        assert_eq!(cx.sleeps.get(), 3);
    }

    #[test]
    fn closed_fd_gets_nval_and_counts() {
        let cx = TestContext::with(&[(3, IDLE)]);
        let mut fds = [entry(3, PollEvents::IN), entry(9, PollEvents::IN)];
        assert_eq!(sys_poll(&cx, &mut fds, None), Ok(1));
        assert_eq!(fds[0].revents, PollEvents::empty());
        assert_eq!(fds[1].revents, PollEvents::NVAL);
    }

    #[test]
    fn negative_fd_is_skipped() {
        let cx = TestContext::with(&[(3, READABLE)]);
        let mut fds = [entry(-1, PollEvents::IN), entry(3, PollEvents::IN)];
        fds[0].revents = PollEvents::IN;
        assert_eq!(sys_poll(&cx, &mut fds, Some(Duration::ZERO)), Ok(1));
        assert_eq!(fds[0].revents, PollEvents::empty());
        assert_eq!(fds[1].revents, PollEvents::IN);

        let mut only_negative = [entry(-5, PollEvents::IN)];
        assert_eq!(
            sys_poll(&cx, &mut only_negative, Some(Duration::ZERO)),
            Ok(0)
        );
    }

    #[test]
    fn eintr_propagates_from_the_sleep() {
        let cx = TestContext {
            should_interrupt_sleep: true,
            ..TestContext::with(&[(3, IDLE)])
        };
        let mut fds = [entry(3, PollEvents::IN)];
        assert_eq!(sys_poll(&cx, &mut fds, None), Err(EINTR));
    }

    #[test]
    fn hup_shows_without_being_requested() {
        let hung_up = Readiness {
            readable: true,
            writable: false,
            hung_up: true,
        };
        let cx = TestContext::with(&[(3, hung_up)]);
        let mut fds = [entry(3, PollEvents::OUT)];
        assert_eq!(sys_poll(&cx, &mut fds, None), Ok(1));
        assert_eq!(fds[0].revents, PollEvents::HUP);
    }

    #[test]
    fn no_entries_sleep_until_the_deadline() {
        let cx = TestContext::default();
        assert_eq!(
            sys_poll(&cx, &mut [], Some(Duration::from_millis(3))),
            Ok(0)
        );
        assert_eq!(cx.sleeps.get(), 3);
    }
}
