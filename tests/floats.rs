//! End-to-end test for x87 and SSE save and restore across context switches
//! and signal delivery.
//!
//! Boots the shared generic `test-kernel` under QEMU with a `/spawn` manifest
//! that launches `/bin/floats` twice. Each instance loads known x87 and SSE
//! state derived from its own pid, spins so the scheduler preempts it, then
//! compares the registers bit exactly. It also raises `SIGUSR1` on itself with
//! a handler that clobbers every XMM register, so `sigreturn` must restore the
//! pre signal values.
//!
//! Two preconditions are load bearing and a future editor can destroy either
//! without any check failing.
//!
//! Two instances must run, deriving different values from their pids. With a
//! single floating point task nothing else ever holds live FPU registers, so a
//! save that writes the wrong task's state has nothing wrong to write and a
//! fully broken save and restore still looks correct.
//!
//! The guest must have one vcpu. On the default four the two instances land on
//! separate CPUs, never share an FPU, and again nothing detects a broken save.
//! Verified by mutation. Reverting the `CR0.TS` ownership guard on the
//! scheduler's FPU save fails `xmm` on one vcpu and passes on four.

use test_support::{KernelTest, host_env};

#[test]
fn floats() {
    let report = KernelTest::new("floats", host_env!())
        .qemu_args(["-smp", "1"])
        .run();

    report.assert_no_line_contains("floats: FAIL");

    report.assert_line_contains("floats: xmm ok");
    report.assert_line_contains("floats: x87 ok");
    report.assert_line_contains("floats: mxcsr ok");
    report.assert_line_contains("floats: rounding ok");
    report.assert_line_contains("floats: special ok");
    report.assert_line_contains("floats: sum ok");
    report.assert_line_contains("floats: signal ok");

    report.assert_exit_code(0, 0);
    report.assert_exit_code(1, 0);
}
