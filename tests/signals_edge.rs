//! End-to-end signal edge-case test.
//!
//! Boots the shared generic `test-kernel` under QEMU with a two-line `/spawn`
//! manifest that launches two instances of a dedicated test init at
//! `/bin/init`. The harness boots the images its Bazel target declared and
//! parses the serial transcript.
//!
//! Role A walks the hostile syscall paths in order: the `kill(pid, 0)`
//! existence probe, EINVAL and EFAULT returns from every signal syscall,
//! unblockable SIGKILL and SIGSTOP, sigaction old-pointer copy-out, RESETHAND,
//! default deferral plus redelivery of a signal during its own handler,
//! NODEFER nesting, and finally a forged `sigreturn` that must kill the
//! process on SIGSEGV without taking the kernel down. Role B blocks SIGSEGV
//! with a handler installed and faults, which must kill it without ever
//! running that handler. Role C forges a `sigreturn` with no readable frame
//! at its stack pointer, which must also die on SIGSEGV.
//!
//! All three processes die by signal, so the run completes once each reports
//! its outcome. The production kernel and init stay free of test-only
//! branches.

use test_support::KernelTest;

/// Ordered markers, each expected at or after the previous one.
const MARKERS: [&str; 17] = [
    "A: start",
    "A: sig0 self ok",
    "A: sig0 esrch ok",
    "A: kill einval ok",
    "A: how einval ok",
    "A: protected einval ok",
    "A: restorer einval ok",
    "A: efault new ok",
    "A: sigpending einval ok",
    "A: unblockable ok",
    "A: query ok",
    "A: resethand ok",
    "A: defer ok",
    "A: redeliver ok",
    "A: nodefer nested ok",
    "A: forged sigreturn next",
    "terminating process on signal SIGSEGV (pid 1)",
];

#[test]
fn signal_edge_cases_end_to_end() {
    let report = KernelTest::new("signals_edge", test_support::host_env!()).run();

    report.assert_markers_in_order(&MARKERS);

    // Roles B and C run concurrently with A, so their markers carry no
    // ordering relationship to A's phases.
    report.assert_line_contains("B: blocked fault next");
    report.assert_line_contains("C: forged sigreturn next");
    report.assert_line_contains("terminating process on signal SIGSEGV (pid 2)");
    report.assert_line_contains("terminating process on signal SIGSEGV (pid 3)");
    report.assert_line_contains("outcome pid=1 signal=SIGSEGV");
    report.assert_line_contains("outcome pid=2 signal=SIGSEGV");
    report.assert_line_contains("outcome pid=3 signal=SIGSEGV");

    report.assert_no_line_contains("UNREACHABLE");
}
