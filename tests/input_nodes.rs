//! Runs `/bin/tests/input-nodes` with a virtio mouse and a virtio keyboard.
//! The host injects events through the QMP socket while the guest runs.

use std::thread;

use test_support::{KernelTest, host_env, qmp};

/// The X delta of each injected motion event. The guest prints the value it
/// reads, and the test checks it against this.
const MOTION_X: i32 = 5;

#[test]
fn input_nodes() {
    let env = host_env!();
    let socket = qmp::socket_path("input-nodes");
    let injector = {
        let socket = socket.clone();
        let commands = [qmp::mouse_motion('x', MOTION_X), qmp::key_press("a")];
        thread::spawn(move || qmp::inject_until_exit(&socket, &commands))
    };
    let report = KernelTest::new("input_nodes", env)
        .qemu_args([
            "-device".to_owned(),
            "virtio-mouse-pci".to_owned(),
            "-device".to_owned(),
            "virtio-keyboard-pci".to_owned(),
        ])
        .qemu_args(qmp::qemu_args(&socket))
        .run();
    injector.join().expect("qmp injector thread");
    report.assert_no_line_contains("FAIL");
    report.assert_line_contains(&format!("input-nodes: motion x value={MOTION_X}"));
    report.assert_line_contains("input-nodes: key a value=1");
    report.assert_line_contains("input-nodes: second reader motions=");
    report.assert_line_contains("input-nodes: ok");
    report.assert_exit_code(0, 0);
}
