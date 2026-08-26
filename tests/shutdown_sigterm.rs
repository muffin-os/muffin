//! Proves the SYS_SHUTDOWN quiesce protocol runs userspace SIGTERM handlers
//! before the machine loses power.
//!
//! pid 1 installs a SIGTERM handler and parks in nanosleep. pid 2 waits for
//! that installation to settle, then issues SYS_SHUTDOWN. The kernel broadcasts
//! SIGTERM and waits for the victim to exit, so the handler's marker line is
//! written strictly before the exit that releases the grace wait, which is
//! itself strictly before the PM1a write. Seeing the marker in the transcript
//! of a run that ended in a real poweroff is therefore ordering proof, not a
//! coincidence of timing.

use test_support::{KernelTest, host_env};

#[test]
fn sigterm_handler_runs_before_poweroff() {
    let transcript = KernelTest::new("shutdown_sigterm", host_env!()).run_until_poweroff();

    assert!(
        transcript
            .iter()
            .any(|line| line.contains("term-witness: sigterm received")),
        "SIGTERM handler never ran before poweroff, transcript: {transcript:#?}"
    );
}
