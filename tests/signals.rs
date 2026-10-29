//! End-to-end signal delivery test.
//!
//! Boots the shared generic `test-kernel` under QEMU with two `/bin/tests`
//! entries that run two instances of `/bin/signals-init` (a driver and
//! a victim). The harness boots the images its Bazel target declared and
//! parses the serial transcript.
//!
//! The two processes coordinate with a ping/ack handshake over SIGWINCH and
//! SIGURG instead of racing on wall-clock delays. The driver pings the victim
//! and counts the replies it gets back across each phase, then prints one
//! counter report that this test asserts verbatim. A reply can only arrive
//! when the victim is actually scheduled, so the counts prove stop, continue,
//! and terminate really took effect.
//!
//! This exercises masking, sigpending, handler entry, sigreturn restore,
//! remote stop/continue, remote default-terminate, and catchable SIGSEGV. The
//! production kernel and init stay free of test-only branches.

use test_support::KernelTest;

/// Ordered markers, each expected at or after the previous one.
const MARKERS: [&str; 6] = [
    "A: start",
    "A: pending ok",
    "A: after unblock",
    "A: report ready=1 pre=1 stop=0 cont=1 term=0",
    "A: efault ok",
    "A: segv handled",
];

#[test]
fn signal_delivery_end_to_end() {
    let report = KernelTest::new("signals", test_support::host_env!()).run();

    // The report line is the load-bearing assertion. A missing report and a
    // wrong-count report both break the ordered scan the same way, so diagnose
    // them here where the actual line is visible.
    // The stop and terminate counts are negative checks that would also read
    // zero if B never ran, so B's own announcement is required first.
    report.assert_line_contains("B: running");
    report.assert_line_contains("A: report ");
    report.assert_line_contains("A: handler ran");
    if !report
        .transcript
        .iter()
        .any(|line| line.contains(MARKERS[3]))
        && let Some(actual) = report
            .transcript
            .iter()
            .find(|line| line.contains("A: report "))
    {
        panic!(
            "report line did not match {:?}, found {actual:?}",
            MARKERS[3]
        );
    }

    report.assert_markers_in_order(&MARKERS);
    report.assert_line_contains("outcome pid=3 signal=SIGTERM");
    report.assert_no_line_contains("A: UNREACHABLE");
}
