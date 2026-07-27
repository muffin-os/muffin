//! End-to-end signal delivery test.
//!
//! Boots the shared generic `test-kernel` under QEMU with a two-line `/spawn`
//! manifest that launches two instances of a dedicated test init (a driver and
//! a victim) at `/bin/init`. The harness assembles the ISO and disk image and
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
const MARKERS: [&str; 10] = [
    "A: start",
    "A: pending ok",
    "A: handler ran",
    "A: after unblock",
    "stopping process",
    "continuing process",
    "terminating process on signal SIGTERM",
    "A: report ready=1 pre=1 stop=0 cont=1 term=0",
    "A: efault ok",
    "A: segv handled",
];

#[test]
fn signal_delivery_end_to_end() {
    let report = KernelTest::new("signals", test_support::host_env!())
        .program(
            "bin/init",
            env!("CARGO_BIN_FILE_SIGNALS_TEST_INIT_signals-test-init"),
        )
        .spawn("/bin/init")
        .spawn("/bin/init")
        .run();

    // The report line is the load-bearing assertion. A missing report and a
    // wrong-count report both break the ordered scan the same way, so diagnose
    // them here where the actual line is visible.
    report.assert_line_contains("A: report ");
    if !report
        .transcript
        .iter()
        .any(|line| line.contains(MARKERS[7]))
        && let Some(actual) = report
            .transcript
            .iter()
            .find(|line| line.contains("A: report "))
    {
        eprintln!(
            "===== QEMU serial transcript ({} lines) =====",
            report.transcript.len()
        );
        for line in &report.transcript {
            eprintln!("{line}");
        }
        eprintln!("===== end transcript =====");
        panic!(
            "report line did not match {:?}, found {actual:?}",
            MARKERS[7]
        );
    }

    report.assert_markers_in_order(&MARKERS);
    report.assert_no_line_contains("A: UNREACHABLE");
}
