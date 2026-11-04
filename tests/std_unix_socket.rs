//! End-to-end test for `std::os::unix::net` on muffin, driven by
//! `/bin/tests/std-unix-socket`. The echo phase blocks both sides on every
//! message, which finds a wakeup that loses bytes the reader has consumed.

use test_support::{KernelTest, host_env};

#[test]
fn std_unix_socket() {
    let report = KernelTest::new("std_unix_socket", host_env!()).run();

    report.assert_no_line_contains("std-unix-socket: FAIL");
    report.assert_markers_in_order(&[
        "std-unix-socket: server listening",
        "std-unix-socket: client connected",
        "std-unix-socket: client 20000 round trips ok",
        "std-unix-socket: server got hello from client",
        "std-unix-socket: client got hello from server",
        "std-unix-socket: server child exit=0",
    ]);
    report.assert_exit_code(0, 0);
}
