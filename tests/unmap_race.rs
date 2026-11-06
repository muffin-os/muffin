//! This test checks `munmap` from end to end while a sibling thread is in a syscall that
//! uses the unmapped memory. `/bin/tests/unmap-race` races `read`, `poll` and
//! `waitpid` against `munmap`. The kernel must not panic, and each call must
//! succeed or fail with `EFAULT`.

use test_support::{KernelTest, host_env};

#[test]
fn unmap_race() {
    let report = KernelTest::new("unmap_race", host_env!()).run();

    report.assert_line_contains("unmap-race: done");
    report.assert_no_line_contains("FAIL");
    report.assert_exit_code(0, 0);
}
