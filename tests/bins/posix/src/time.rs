use minilib::{
    CLOCK_MONOTONIC, CLOCK_REALTIME, EFAULT, EINVAL, SYS_CLOCK_GETTIME, SYS_NANOSLEEP, Timespec,
    clock_gettime, nanosleep, ret, syscall2,
};

use crate::check;

const UNKNOWN_CLOCK: usize = 4;

const KERNEL_PTR: usize = 0xFFFF_8000_0000_0000;

const NS_PER_SEC: i64 = 1_000_000_000;
const SLEEP_NS: i64 = 20_000_000;
const MAX_ELAPSED_NS: i64 = 2 * NS_PER_SEC;

fn zero() -> Timespec {
    Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    }
}

fn monotonic(name: &str) -> Timespec {
    let mut ts = zero();
    check::unwrap_or_fail(name, clock_gettime(CLOCK_MONOTONIC, &mut ts));
    ts
}

fn total_ns(ts: &Timespec) -> i64 {
    ts.tv_sec * NS_PER_SEC + ts.tv_nsec
}

pub fn run() {
    check::group("time");

    let first = monotonic("time/monotonic_ok");
    check::require(
        "time/monotonic_nsec_range",
        (0..NS_PER_SEC).contains(&first.tv_nsec),
    );

    let second = monotonic("time/monotonic_second_ok");
    check::require(
        "time/monotonic_nondecreasing",
        total_ns(&second) >= total_ns(&first),
    );

    let mut realtime = zero();
    check::unwrap_or_fail(
        "time/realtime_ok",
        clock_gettime(CLOCK_REALTIME, &mut realtime),
    );
    check::require(
        "time/realtime_nsec_range",
        (0..NS_PER_SEC).contains(&realtime.tv_nsec),
    );
    check::require(
        "time/realtime_at_or_after_monotonic",
        realtime.tv_sec >= second.tv_sec,
    );

    let mut sink = zero();
    check::expect_err(
        "time/unknown_clock",
        clock_gettime(UNKNOWN_CLOCK, &mut sink),
        EINVAL,
    );
    check::expect_err(
        "time/gettime_null_tp",
        ret(syscall2(SYS_CLOCK_GETTIME, CLOCK_MONOTONIC, 0)),
        EFAULT,
    );
    check::expect_err(
        "time/gettime_kernel_tp",
        ret(syscall2(SYS_CLOCK_GETTIME, CLOCK_MONOTONIC, KERNEL_PTR)),
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
    check::expect_err(
        "time/sleep_null_req",
        ret(syscall2(SYS_NANOSLEEP, 0, 0)),
        EFAULT,
    );
    check::expect_err(
        "time/sleep_kernel_req",
        ret(syscall2(SYS_NANOSLEEP, KERNEL_PTR, 0)),
        EFAULT,
    );

    check::expect_ok("time/sleep_zero", nanosleep(&zero(), None), ());

    let before = monotonic("time/sleep_before_ok");
    check::expect_ok(
        "time/sleep_20ms",
        nanosleep(
            &Timespec {
                tv_sec: 0,
                tv_nsec: SLEEP_NS,
            },
            None,
        ),
        (),
    );
    let after = monotonic("time/sleep_after_ok");
    let elapsed = total_ns(&after) - total_ns(&before);
    check::require("time/sleep_20ms_lower_bound", elapsed >= SLEEP_NS);
    check::require("time/sleep_20ms_upper_bound", elapsed <= MAX_ELAPSED_NS);
}
