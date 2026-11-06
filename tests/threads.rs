//! This test checks threads in the Rust standard library on muffin from end to end.
//!
//! `threads` prints one marker per step. `threads-exit` ends the process from
//! a spawned thread while the main thread joins it. `threads-last` ends the
//! main thread with `pthread_exit` while a spawned thread still runs.
//! Each of `stack-overflow-thread` and `stack-overflow-main` recurses without end.
//! The kernel must end each of them with `SIGSEGV`. In `fork-threads`, a fork child reads the TLS block
//! of a sibling thread, which the child does not own.

use test_support::{KernelTest, host_env};

#[test]
fn threads() {
    let report = KernelTest::new("threads", host_env!()).run();

    report.assert_markers_in_order(&[
        "threads: spawn join ok",
        "threads: self id ok",
        "threads: mutex counter ok",
        "threads: channel ok",
        "threads: condvar ok",
        "threads: thread local ok",
        "threads: sigmask ok",
        "threads: detach ok",
        "threads: done",
    ]);
    report.assert_no_line_contains("threads: FAIL");
    report.assert_exit_code(0, 0);

    report.assert_line_contains("threads-exit: worker exits");
    report.assert_exit_code(1, 7);

    report.assert_markers_in_order(&["threads-last: main exits", "threads-last: worker done"]);
    report.assert_exit_code(2, 0);

    report.assert_line_contains("stack-overflow-thread: start");
    report.assert_signaled(3, "SIGSEGV");

    report.assert_line_contains("stack-overflow-main: start");
    report.assert_signaled(4, "SIGSEGV");

    report.assert_line_contains("fork-threads: sibling tls unmapped ok");
    report.assert_exit_code(5, 0);
}
