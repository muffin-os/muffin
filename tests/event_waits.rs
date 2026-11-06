//! End-to-end test for waits that end on events instead of periodic retries.
//!
//! `/bin/tests/event-waits` runs each `poll` and `waitpid` case in a fresh
//! child process. The `/adoption-probe` marker makes the test kernel prove,
//! before init starts, that pid 1 wakes for and reaps an adopted zombie.
//! Each guest runs once with one CPU and once with four CPUs.

use test_support::{KernelTest, host_env};

fn run(name: &'static str, cpus: &str) {
    // The name selects the work directory, so parallel runs need distinct names.
    let report = KernelTest::new(name, host_env!())
        .qemu_args(["-smp", cpus])
        .run();

    report.assert_line_contains("event-waits: adopted zombie ok");
    report.assert_no_line_contains("event-waits: FAIL");
    report.assert_markers_in_order(&[
        "event-waits: readiness ok",
        "event-waits: either socket ok",
        "event-waits: duplicate entries ok",
        "event-waits: peer close hup ok",
        "event-waits: close nval ok",
        "event-waits: replacement ok",
        "event-waits: two child waiters ok",
        "event-waits: signal eintr ok",
        "event-waits: absolute deadline ok",
        "event-waits: empty deadline ok",
        "event-waits: ok",
    ]);
    report.assert_exit_code(0, 0);
}

#[test]
fn event_waits_one_cpu() {
    run("event_waits_smp1", "1");
}

#[test]
fn event_waits_four_cpus() {
    run("event_waits_smp4", "4");
}
