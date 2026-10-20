use core::time::Duration;

use minilib::{
    ClockId, EFAULT, EINVAL, SYS_CLOCK_GETTIME, SYS_NANOSLEEP, Timespec, clock_gettime, nanosleep,
    ret, syscall2,
};

use crate::check;

const UNKNOWN_CLOCK: usize = 4;

const KERNEL_PTR: usize = 0xFFFF_8000_0000_0000;

const NS_PER_SEC: i64 = 1_000_000_000;
const SLEEP: Duration = Duration::from_millis(20);
const MAX_ELAPSED: Duration = Duration::from_secs(2);

fn gettime(name: &str, clockid: ClockId) -> Duration {
    let mut ts = Timespec::default();
    check::unwrap_or_fail(name, clock_gettime(clockid, &mut ts));
    check::unwrap_or_fail(name, Duration::try_from(ts))
}

pub fn run() {
    check::group("time");

    let first = gettime("time/monotonic_ok", ClockId::Monotonic);
    let second = gettime("time/monotonic_second_ok", ClockId::Monotonic);
    check::require("time/monotonic_nondecreasing", second >= first);

    let realtime = gettime("time/realtime_ok", ClockId::Realtime);
    check::require(
        "time/realtime_at_or_after_monotonic",
        realtime.as_secs() >= second.as_secs(),
    );

    // Only the raw syscall can hand the kernel a clockid `ClockId` cannot name.
    let mut sink = Timespec::default();
    let sink_ptr = core::ptr::from_mut(&mut sink) as usize;
    check::expect_err(
        "time/unknown_clock",
        ret(unsafe { syscall2(SYS_CLOCK_GETTIME, UNKNOWN_CLOCK, sink_ptr) }),
        EINVAL,
    );
    // Safety: these pointers are deliberately invalid, and the kernel rejecting
    // them before any access is what each check asserts.
    check::expect_err(
        "time/gettime_null_tp",
        ret(unsafe { syscall2(SYS_CLOCK_GETTIME, ClockId::Monotonic as usize, 0) }),
        EFAULT,
    );
    check::expect_err(
        "time/gettime_kernel_tp",
        ret(unsafe { syscall2(SYS_CLOCK_GETTIME, ClockId::Monotonic as usize, KERNEL_PTR) }),
        EFAULT,
    );

    check::expect_err(
        "time/sleep_negative_sec",
        nanosleep(
            &Timespec {
                tv_sec: -1,
                tv_nsec: 0,
            },
            None,
        ),
        EINVAL,
    );
    check::expect_err(
        "time/sleep_negative_nsec",
        nanosleep(
            &Timespec {
                tv_sec: 0,
                tv_nsec: -1,
            },
            None,
        ),
        EINVAL,
    );
    check::expect_err(
        "time/sleep_nsec_at_limit",
        nanosleep(
            &Timespec {
                tv_sec: 0,
                tv_nsec: NS_PER_SEC,
            },
            None,
        ),
        EINVAL,
    );
    // Safety: these pointers are deliberately invalid, and the kernel rejecting
    // them before any access is what each check asserts.
    check::expect_err(
        "time/sleep_null_req",
        ret(unsafe { syscall2(SYS_NANOSLEEP, 0, 0) }),
        EFAULT,
    );
    check::expect_err(
        "time/sleep_kernel_req",
        ret(unsafe { syscall2(SYS_NANOSLEEP, KERNEL_PTR, 0) }),
        EFAULT,
    );

    check::expect_ok("time/sleep_zero", nanosleep(&Timespec::default(), None), ());

    let before = gettime("time/sleep_before_ok", ClockId::Monotonic);
    check::expect_ok("time/sleep_20ms", nanosleep(&SLEEP.into(), None), ());
    let after = gettime("time/sleep_after_ok", ClockId::Monotonic);
    let elapsed = after.saturating_sub(before);
    check::require("time/sleep_20ms_lower_bound", elapsed >= SLEEP);
    check::require("time/sleep_20ms_upper_bound", elapsed <= MAX_ELAPSED);
}
