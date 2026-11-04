//! This test boots the guest and runs `/bin/tests/shm`.
//!
//! The program prints `shm: ok` and exits 0 only when every step passes. The
//! steps cover `shm_open`, `set_len`, shared `mmap`, `munmap` and
//! `shm_unlink`, as POSIX describes them.

use test_support::{KernelTest, host_env};

#[test]
fn shm() {
    let report = KernelTest::new("shm", host_env!()).run();

    report.assert_no_line_contains("FAIL");
    report.assert_line_contains("shm: ok");
    report.assert_exit_code(0, 0);
}
