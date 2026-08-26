//! Proves signal delivery on the syscall return path. pid 1 installs a
//! SIGTERM handler and waits inside nanosleep, so it spends its time in the
//! kernel and a user-mode timer tick can never deliver. pid 2 settles, then
//! issues SYS_SHUTDOWN. The broadcast wakes the sleeper, nanosleep fails
//! EINTR, and the marker in the transcript can only come from delivery on
//! the syscall return.

use test_support::{KernelTest, host_env};

#[test]
fn sigterm_handler_runs_during_nanosleep() {
    let transcript = KernelTest::new("shutdown_sigterm_sleep", host_env!()).run_until_poweroff();

    assert!(
        transcript
            .iter()
            .any(|line| line.contains("term-sleeper: sigterm received")),
        "SIGTERM handler never ran while victim slept, transcript: {transcript:#?}"
    );
}
