//! Runs `/bin/tests/input-nodes` with a virtio mouse and a virtio keyboard.
//! The guest prints a request marker for each phase. The host then injects
//! the events of that phase through the QMP socket.

use test_support::{KernelTest, host_env, qmp};

/// The X delta of each injected motion event. The guest checks the value it
/// reads against this.
const MOTION_X: i32 = 5;
/// The guest requires both mouse readers to get this number of motions.
const BATCH_MOTIONS: usize = 10;

#[test]
fn input_nodes() {
    let env = host_env!();
    let socket = qmp::socket_path("input-nodes");
    let mut qmp = None;
    let report = KernelTest::new("input_nodes", env)
        .qemu_args([
            "-device".to_owned(),
            "virtio-mouse-pci".to_owned(),
            "-device".to_owned(),
            "virtio-keyboard-pci".to_owned(),
        ])
        .qemu_args(qmp::qemu_args(&socket))
        .run_observing(|line| {
            let commands = if line.contains("input-nodes: await mouse") {
                vec![qmp::mouse_motion('x', MOTION_X)]
            } else if line.contains("input-nodes: await keyboard") {
                vec![qmp::key_press("a")]
            } else if line.contains("input-nodes: await batch") {
                vec![qmp::mouse_motion('x', MOTION_X); BATCH_MOTIONS]
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
    report.assert_markers_in_order(&[
        "input-nodes: await mouse",
        &format!("input-nodes: motion x value={MOTION_X}"),
        "input-nodes: await keyboard",
        "input-nodes: key a value=1",
        "input-nodes: await batch",
        &format!("input-nodes: second reader motions={BATCH_MOTIONS}"),
        "input-nodes: ok",
    ]);
    report.assert_exit_code(0, 0);
}
