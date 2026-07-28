/// POSIX `clockid_t` values accepted by `clock_gettime`.
///
/// The system-wide wall clock. Measures seconds and nanoseconds since the
/// Epoch (1970-01-01 00:00:00 UTC).
pub const CLOCK_REALTIME: usize = 0;
/// A clock that cannot be set and never jumps backwards. Measures time since
/// boot.
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
