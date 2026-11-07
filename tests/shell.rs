//! Runs the compositor with `--probe` and the shell on a virgl GPU with a
//! virtio keyboard. The host types the commands through QMP when the shell
//! is ready. The last command, `poweroff`, ends the run.
//!
//! The probe of the first blit shows that the CPU-drawn window reaches the
//! scanout at its centred position.

use test_support::{KernelTest, host_env, qmp};

const COMMANDS: [&str; 8] = [
    "echo hello world",
    "cd /bin",
    "ls",
    "cd shell",
    "cd ..",
    "foo",
    "tree /bin",
    "poweroff",
];

/// Returns a press and a release of the key with the QEMU code `qcode` as
/// one command.
fn key_stroke(qcode: &str) -> String {
    let event = |is_down: bool| {
        format!(
            "{{\"type\":\"key\",\"data\":{{\"down\":{is_down},\"key\":{{\"type\":\"qcode\",\"data\":\"{qcode}\"}}}}}}"
        )
    };
    qmp::input_event(&format!("{},{}", event(true), event(false)))
}

fn qcode(c: char) -> String {
    match c {
        ' ' => "spc".to_owned(),
        '/' => "slash".to_owned(),
        '.' => "dot".to_owned(),
        '\n' => "ret".to_owned(),
        c => c.to_string(),
    }
}

#[test]
fn shell() {
    let socket = qmp::socket_path("shell");
    let mut qmp = None;
    let transcript = KernelTest::new("shell", host_env!())
        .virgl_gpu(800, 600)
        .qemu_args(["-device".to_owned(), "virtio-keyboard-pci".to_owned()])
        .qemu_args(qmp::qemu_args(&socket))
        .run_until_poweroff_observing(|line| {
            if !line.contains("shell: ready") {
                return Ok(());
            }
            let commands: Vec<String> = COMMANDS
                .iter()
                .flat_map(|command| command.chars().chain(['\n']))
                .map(|c| key_stroke(&qcode(c)))
                .collect();
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

    assert!(
        !transcript.iter().any(|line| line.contains("FAIL")),
        "a line contains FAIL"
    );
    let mut lines = transcript.iter();
    for marker in [
        "compositor: new window 480x360",
        "shell: window 480x360 at (160,120)",
        "compositor: blit 480x360 first=0xff1e2430",
        "shell: /> echo hello world",
        "shell: hello world",
        "shell: /> cd /bin",
        "shell: /bin> ls",
        "shell: compositor",
        "shell: init",
        "shell: shell",
        "shell: tests/",
        "shell: /bin> cd shell",
        "shell: cd: shell: not a directory",
        "shell: /bin> cd ..",
        "shell: /> foo",
        "shell: unknown command: foo",
        "shell: /> tree /bin",
        "shell: /bin",
        "shell: |-- compositor",
        "shell: |-- shell",
        "shell: `-- tests",
        "shell:     `-- shell-smoke",
        "shell: 1 directories, 4 files",
        "shell: powering off",
    ] {
        assert!(
            lines.any(|line| line.contains(marker)),
            "marker {marker:?} not found after the previous marker"
        );
    }
}
