//! End-to-end nanosleep test.
//!
//! Boots the shared generic `test-kernel` under QEMU with a two-line `/spawn`
//! manifest that launches two instances of a dedicated test init at
//! `/bin/init`. The victim first proves the happy path, a 200ms sleep that
//! parks the task and returns 0 only after the monotonic clock moved at least
//! 200ms. It then parks in a 10s sleep with a SIGURG handler installed, and
//! the driver interrupts that sleep with a kill after a settle delay, which
//! must surface as `EINTR` long before the deadline.

use test_support::{KernelTest, host_env};

const MARKERS: [&str; 3] = ["sleep: start", "sleep: duration ok", "sleep: eintr ok"];

#[test]
fn nanosleep_end_to_end() {
    let report = KernelTest::new("sleep", host_env!()).run();

    report.assert_markers_in_order(&MARKERS);
    report.assert_no_line_contains("sleep: UNREACHABLE");
}
