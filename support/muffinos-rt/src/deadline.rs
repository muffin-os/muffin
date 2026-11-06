use core::time::Duration;

use kernel_abi::{Errno, Timespec};

/// This function returns the time from `now` until `deadline`. It returns
/// `None` when `deadline` is not after `now`. It fails with `EINVAL` for a
/// `timespec` outside the POSIX range.
pub(crate) fn time_between(now: Timespec, deadline: Timespec) -> Result<Option<Duration>, Errno> {
    let deadline = Duration::try_from(deadline)?;
    let now = Duration::try_from(now)?;
    Ok(deadline.checked_sub(now).filter(|left| !left.is_zero()))
}

#[cfg(test)]
mod tests {
    use kernel_abi::EINVAL;

    use super::*;

    fn at(tv_sec: i64, tv_nsec: i64) -> Timespec {
        Timespec { tv_sec, tv_nsec }
    }

    #[test]
    fn future_deadline_leaves_the_difference() {
        assert_eq!(
            time_between(at(10, 900_000_000), at(12, 100_000_000)),
            Ok(Some(Duration::from_millis(1200))),
            "the difference borrows a second from tv_sec"
        );
        assert_eq!(
            time_between(at(5, 0), at(5, 1)),
            Ok(Some(Duration::from_nanos(1))),
            "one nanosecond before the deadline is still a wait"
        );
    }

    #[test]
    fn reached_or_past_deadline_leaves_nothing() {
        assert_eq!(
            time_between(at(5, 7), at(5, 7)),
            Ok(None),
            "now is the deadline"
        );
        assert_eq!(
            time_between(at(9, 0), at(5, 0)),
            Ok(None),
            "the deadline passed"
        );
    }

    #[test]
    fn deadline_outside_the_posix_range_fails() {
        assert_eq!(
            time_between(at(5, 0), at(6, 1_000_000_000)),
            Err(EINVAL),
            "tv_nsec must stay below one second"
        );
        assert_eq!(
            time_between(at(5, 0), at(-1, 0)),
            Err(EINVAL),
            "tv_sec must not be negative"
        );
    }
}
