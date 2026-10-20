use core::time::Duration;

use crate::{EINVAL, Errno};

/// The `clockid_t` of the system-wide wall clock, seconds and nanoseconds
/// since the Epoch (1970-01-01 00:00:00 UTC).
pub const CLOCK_REALTIME: usize = 0;
/// The `clockid_t` of a clock that cannot be set and never jumps backwards.
/// Measures time since boot.
pub const CLOCK_MONOTONIC: usize = 1;

/// POSIX `struct timespec` as defined in `<time.h>`.
///
/// `tv_nsec` is valid in `0..1_000_000_000`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

impl From<Duration> for Timespec {
    /// Saturates `tv_sec` at `i64::MAX` for durations beyond the POSIX range.
    fn from(duration: Duration) -> Self {
        Self {
            tv_sec: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(duration.subsec_nanos()),
        }
    }
}

impl TryFrom<Timespec> for Duration {
    type Error = Errno;

    /// Fails with `EINVAL` for a negative `tv_sec` or a `tv_nsec` outside
    /// `0..1_000_000_000`, the same values nanosleep rejects.
    fn try_from(ts: Timespec) -> Result<Self, Errno> {
        let secs = u64::try_from(ts.tv_sec).map_err(|_| EINVAL)?;
        let nanos = u32::try_from(ts.tv_nsec)
            .ok()
            .filter(|nanos| *nanos < 1_000_000_000)
            .ok_or(EINVAL)?;
        Ok(Self::new(secs, nanos))
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;
    use crate::EINVAL;

    #[test]
    fn timespec_from_duration_splits_subsecond_part() {
        assert_eq!(
            Timespec::from(Duration::new(3, 250_000_000)),
            Timespec {
                tv_sec: 3,
                tv_nsec: 250_000_000,
            },
            "whole seconds go to tv_sec and the remainder to tv_nsec"
        );
    }

    #[test]
    fn timespec_from_duration_saturates_seconds() {
        assert_eq!(
            Timespec::from(Duration::MAX).tv_sec,
            i64::MAX,
            "seconds beyond i64 saturate instead of wrapping negative"
        );
    }

    #[test]
    fn duration_round_trips_through_timespec() {
        let duration = Duration::new(u64::try_from(i64::MAX).unwrap(), 999_999_999);
        assert_eq!(
            Duration::try_from(Timespec::from(duration)),
            Ok(duration),
            "every non-negative timespec with a valid tv_nsec is a Duration"
        );
    }

    #[test]
    fn duration_rejects_timespec_outside_posix_range() {
        for invalid in [
            Timespec {
                tv_sec: -1,
                tv_nsec: 0,
            },
            Timespec {
                tv_sec: 0,
                tv_nsec: -1,
            },
            Timespec {
                tv_sec: 0,
                tv_nsec: 1_000_000_000,
            },
        ] {
            assert_eq!(
                Duration::try_from(invalid),
                Err(EINVAL),
                "{invalid:?} is not a valid POSIX timespec"
            );
        }
    }
}
