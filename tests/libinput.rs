//! Runs `/bin/tests/libinput` with a virtio mouse and a virtio keyboard. The
//! guest reads both devices through `libinput` and prints a request marker
//! for each phase. The host then injects the events of that phase through
//! the QMP socket.

use test_support::{KernelTest, host_env, qmp};

fn key(qcode: &str, is_down: bool) -> String {
    qmp::input_event(&format!(
        "{{\"type\":\"key\",\"data\":{{\"down\":{is_down},\"key\":{{\"type\":\"qcode\",\"data\":\"{qcode}\"}}}}}}"
    ))
}

#[test]
fn libinput() {
    let socket = qmp::socket_path("libinput");
    let mut qmp = None;
    // One command is one batch. Thus the guest gets one motion with both axes.
    let motion = qmp::input_event(concat!(
        r#"{"type":"rel","data":{"axis":"x","value":5}},"#,
        r#"{"type":"rel","data":{"axis":"y","value":-3}}"#,
    ));
    let report = KernelTest::new("libinput", host_env!())
        .qemu_args([
            "-device".to_owned(),
            "virtio-mouse-pci".to_owned(),
            "-device".to_owned(),
            "virtio-keyboard-pci".to_owned(),
        ])
        .qemu_args(qmp::qemu_args(&socket))
        .run_observing(|line| {
            let commands = if line.contains("libinput: await mouse") {
                vec![
                    motion.clone(),
                    qmp::mouse_button("left", true),
                    qmp::mouse_button("left", false),
                ]
            } else if line.contains("libinput: await keyboard") {
                vec![
                    key("shift", true),
                    key("a", true),
                    key("a", false),
                    key("shift", false),
                ]
            } else {
                return Ok(());
            };
            let qmp = qmp.get_or_insert_with(|| qmp::Qmp::connect(&socket));
            for command in &commands {
                let reply = qmp
                    .send(command)
                    .ok_or_else(|| format!("qmp closed before {line:?}"))?;
                if !reply.contains("\"return\"") {
                    return Err(format!("qmp rejected {command}: {reply}"));
                }
            }
            Ok(())
        });
    report.assert_no_line_contains("FAIL");
    let mouse = device_id(&report.transcript, "Mouse");
    let keyboard = device_id(&report.transcript, "Keyboard");
    assert_ne!(mouse, keyboard, "each device has its own id");
    report.assert_markers_in_order(&[
        "libinput: await mouse",
        &format!("libinput: event {mouse} Motion {{ dx: 5, dy: -3 }}"),
        &format!("libinput: event {mouse} Button {{ button: Left, state: Pressed }}"),
        &format!("libinput: event {mouse} Button {{ button: Left, state: Released }}"),
        "libinput: await keyboard",
        &format!("libinput: event {keyboard} Key {{ code: KeyCode(42), state: Pressed }}"),
        &format!("libinput: event {keyboard} Key {{ code: KeyCode(30), state: Pressed }}"),
        "libinput: typed \"A\"",
        "libinput: ok",
    ]);
    report.assert_exit_code(0, 0);
}

/// Returns the id in the `libinput: device <id> <path> <name>` line whose
/// device name contains `kind`.
fn device_id<'a>(transcript: &'a [String], kind: &str) -> &'a str {
    transcript
        .iter()
        .filter_map(|line| line.strip_prefix("libinput: device "))
        .find(|line| line.contains(kind))
        .and_then(|line| line.split(' ').next())
        .unwrap_or_else(|| panic!("no device line with {kind:?}"))
}
