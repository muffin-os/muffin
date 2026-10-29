//! This test checks the copy-on-write clone of a process's lower-half
//! memory regions.
//!
//! The test kernel runs the probe when the disk holds the `/cow-clone`
//! marker. The probe clones the regions of one process into a second
//! process. It reports each observable outcome on the serial line.
//!
//! Each partner task masks the timer interrupt on its CPU while it spins.
//! The scheduler then cannot move the task or flush its TLB. Each run
//! exercises the cross-CPU invalidation.

use test_support::{KernelTest, host_env};

#[test]
fn cloned_pages_are_shared_until_written() {
    let report = KernelTest::new("cow_clone", host_env!()).run();

    report.assert_line_contains("cow clone shared_read=ok");
    report.assert_line_contains("cow clone private_write=ok frame_changed=true");
    report.assert_line_contains("cow clone reader_sees_write=ok");
    report.assert_line_contains("cow origin isolated=ok");
    report.assert_line_contains("cow origin break=ok frame_changed=true");
    report.assert_line_contains("cow origin reclaim=ok frame_same=true");
    report.assert_exit_code(0, 42);
}
