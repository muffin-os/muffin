//! End-to-end test for the Rust standard library on muffin.
//!
//! `std-smoke` prints one marker per step. The second entry, `std-panic-exit`,
//! lets a panic escape `main` so the test can assert std's uncaught-panic exit
//! code.

use test_support::{KernelTest, host_env};

#[test]
fn std_smoke() {
    let report = KernelTest::new("std_smoke", host_env!()).run();

    report.assert_markers_in_order(&[
        "std-smoke: args ok",
        "std-smoke: env ok",
        "std-smoke: read ok",
        "std-smoke: write ok",
        "std-smoke: readdir ok",
        "std-smoke: metadata ok",
        "std-smoke: time ok",
        "std-smoke: random ok",
        "std-smoke: exe ok",
        "std-smoke: process ok",
        "std-smoke: child env ok",
        "std-smoke: child inherit ok",
        "std-smoke: child_env ok",
        "std-smoke: panic ok",
        "std-smoke: done",
    ]);
    report.assert_line_contains("std-panic-exit: escape");
    report.assert_no_line_contains("std-smoke: FAIL");
    // fstat used to probe for a directory with getdents, which logged ENOTDIR.
    report.assert_no_line_contains("ENOTDIR");
    report.assert_exit_code(0, 0);
    report.assert_exit_code(1, 101);
}
