//! End-to-end test for the service manager.
//!
//! `services` runs as `/bin/init`. It loads one service per file in
//! `/etc/services`, spawns every service, and applies the restart policies
//! while `off` sleeps and then powers the machine off.

use test_support::{KernelTest, host_env};

#[test]
fn services() {
    let transcript = KernelTest::new("services", host_env!()).run_until_poweroff();
    let has = |needle: &str| transcript.iter().any(|line| line.contains(needle));

    assert!(has("services: parsed 4 services"));
    assert!(has("services: spawned crasher pid="));
    let first = |needle: &str| {
        transcript
            .iter()
            .position(|line| line.contains(needle))
            .expect(needle)
    };
    assert!(
        first("services: spawned once pid=") < first("services: spawned always pid="),
        "after must spawn once before always"
    );
    assert!(
        has("services: restarting crasher"),
        "on-failure must restart a failed service"
    );
    assert!(
        has("services: restarting always"),
        "always must restart a succeeded service"
    );
    assert!(
        !has("services: restarting once"),
        "on-failure must not restart a succeeded service"
    );
    assert!(!has("services: restarting off"));
    assert!(!has("services: FAIL"));
}
