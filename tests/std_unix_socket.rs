//! End-to-end test for `std::os::unix::net` on muffin.
//!
//! `/bin/tests/std-unix-socket` binds a `UnixListener`, spawns itself as the
//! client, and exchanges one line in each direction. The client's
//! `shutdown(Write)` gives the server EOF on `read_line`. The drop of the
//! server stream gives the client EOF on `read_to_string`.

use test_support::{KernelTest, host_env};

#[test]
fn std_unix_socket() {
    let report = KernelTest::new("std_unix_socket", host_env!()).run();

    report.assert_no_line_contains("std-unix-socket: FAIL");
    report.assert_markers_in_order(&[
        "std-unix-socket: server listening",
        "std-unix-socket: client connected",
        "std-unix-socket: server got hello from client",
        "std-unix-socket: client got hello from server",
        "std-unix-socket: server child exit=0",
    ]);
    report.assert_exit_code(0, 0);
}
