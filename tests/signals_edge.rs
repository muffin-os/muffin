//! End-to-end signal edge-case test.
//!
//! Six instances of `/bin/init` take a role each by pid. Role A prints one
//! `ok` marker per handler flag semantics check (RESETHAND, deferral, NODEFER
//! nesting), in `MARKERS` order, and then forges a `sigreturn` that must kill
//! it on SIGSEGV. Plain errno checks on the signal syscalls live in the
//! `posix` suite. Roles B to F each
//! announce a fault the kernel must answer by killing them, B to D and F on
//! SIGSEGV and E on SIGILL, without the kernel going down. The run completes
//! once all six report an outcome, and no role may reach its `UNREACHABLE`
//! line.

use test_support::KernelTest;

/// Ordered markers, each expected at or after the previous one.
const MARKERS: [&str; 7] = [
    "A: start",
    "A: resethand ok",
    "A: defer ok",
    "A: redeliver ok",
    "A: nodefer nested ok",
    "A: forged sigreturn next",
    "outcome pid=1 signal=SIGSEGV",
];

#[test]
fn signal_edge_cases_end_to_end() {
    let report = KernelTest::new("signals_edge", test_support::host_env!()).run();

    report.assert_markers_in_order(&MARKERS);

    // Roles B to F run concurrently with A, so their markers carry no
    // ordering relationship to A's phases.
    report.assert_line_contains("B: blocked fault next");
    report.assert_line_contains("C: forged sigreturn next");
    report.assert_line_contains("D: wild stack delivery next");
    report.assert_line_contains("E: ud2 next");
    report.assert_line_contains("F: hlt next");
    report.assert_line_contains("outcome pid=2 signal=SIGSEGV");
    report.assert_line_contains("outcome pid=3 signal=SIGSEGV");
    report.assert_line_contains("outcome pid=4 signal=SIGSEGV");
    report.assert_line_contains("outcome pid=5 signal=SIGILL");
    report.assert_line_contains("outcome pid=6 signal=SIGSEGV");

    report.assert_no_line_contains("UNREACHABLE");
}
