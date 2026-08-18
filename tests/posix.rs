//! End-to-end POSIX conformance test for the syscall surface.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/posix` in the
//! `/spawn` manifest. The guest binary walks one group per syscall family,
//! pinning return values, argument validation, and errno selection against
//! the real kernel. Each group announces itself as `posix: group <name>`, and
//! each failed check names itself as `posix: FAIL <group>/<case>`.
//!
//! The `execve` group carries the regression guard for a ring-0 spin: an argv
//! or envp element count above `ARG_MAX / size_of::<StrSlice>()` must fail
//! with `E2BIG` from the count check alone, so the kernel never walks the
//! implied page range. A count of 2^36 spans 2^28 pages, and walking them
//! under the region spinlock hangs ring 0 instead of returning, so losing that
//! check fails this suite by harness deadline rather than by assertion.

use test_support::{KernelTest, host_env};

#[test]
fn posix() {
    let report = KernelTest::new("posix", host_env!()).run();

    report.assert_no_line_contains("posix: FAIL");
    report.assert_markers_in_order(&[
        "posix: start",
        "posix: group fd",
        "posix: group mmap",
        "posix: group process",
        "posix: group signal",
        "posix: group time",
        "posix: group execve",
        "posix: all checks passed",
    ]);
    report.assert_exit_code(0, 0);
}
