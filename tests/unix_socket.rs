//! End-to-end test for AF_UNIX stream sockets through `muffin_sys`.
//!
//! `/bin/tests/unix-socket` listens and forks. The child connects while the
//! parent parks in `accept`. The two exchange `ping` and `pong` through
//! blocking reads. After the parent closes its end, the child sees EOF and
//! then `EPIPE`. The child ignores `SIGPIPE`.

use test_support::{KernelTest, host_env};

#[test]
fn unix_socket() {
    let report = KernelTest::new("unix_socket", host_env!()).run();

    report.assert_no_line_contains("unix-socket: FAIL");
    // The parent and the child print from two processes, so their lines have
    // no fixed order.
    for marker in [
        "unix-socket: listening",
        "unix-socket: child connected",
        "unix-socket: accepted",
        "unix-socket: parent got ping",
        "unix-socket: child got pong",
        "unix-socket: child eof",
        "unix-socket: child epipe",
        "unix-socket: parent waited status=0",
    ] {
        report.assert_line_contains(marker);
    }
    report.assert_exit_code(0, 0);
}
