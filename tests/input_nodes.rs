//! Runs `/bin/tests/input-nodes` with a virtio mouse and a virtio keyboard.
//! The host injects events through the QMP socket while the guest runs.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use test_support::{KernelTest, host_env};

const QMP_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
const INJECT_INTERVAL: Duration = Duration::from_millis(50);

/// The X delta of each injected motion event. The guest prints the value it
/// reads, and the test checks it against this.
const MOTION_X: i32 = 5;

/// Connects to the QMP socket once QEMU created it and finishes the handshake.
fn connect_qmp(socket: &Path) -> UnixStream {
    let deadline = Instant::now() + QMP_CONNECT_TIMEOUT;
    let stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("qmp socket did not appear: {e}"),
        }
    };
    let mut reader = BufReader::new(stream.try_clone().expect("clone qmp stream"));
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("qmp greeting");
    let mut stream = stream;
    stream
        .write_all(b"{\"execute\":\"qmp_capabilities\"}\n")
        .expect("qmp capabilities");
    let mut reply = String::new();
    reader
        .read_line(&mut reply)
        .expect("qmp capabilities reply");
    assert!(
        reply.contains("\"return\""),
        "unexpected qmp reply: {reply}"
    );
    stream
}

/// Sends mouse motion and a key press over and over until QEMU closes the
/// socket. QEMU routes each event to the input handler the guest activated
/// last, so the virtio devices receive them only after the guest driver came
/// up, and one shot is not enough.
fn inject_until_exit(socket: &Path) {
    let mut stream = connect_qmp(socket);
    let mut reader = BufReader::new(stream.try_clone().expect("clone qmp stream"));
    let motion = format!(
        "{{\"execute\":\"input-send-event\",\"arguments\":{{\
         \"events\":[{{\"type\":\"rel\",\"data\":{{\"axis\":\"x\",\"value\":{MOTION_X}}}}}]}}}}\n"
    );
    let press = concat!(
        "{\"execute\":\"input-send-event\",\"arguments\":{",
        "\"events\":[{\"type\":\"key\",\"data\":{\"down\":true,",
        "\"key\":{\"type\":\"qcode\",\"data\":\"a\"}}}]}}\n"
    );
    loop {
        for command in [motion.as_str(), press] {
            if stream.write_all(command.as_bytes()).is_err() {
                return;
            }
            let mut reply = String::new();
            if reader.read_line(&mut reply).is_err() || reply.is_empty() {
                return;
            }
        }
        thread::sleep(INJECT_INTERVAL);
    }
}

#[test]
fn input_nodes() {
    let env = host_env!();
    // A unix socket path is limited to 107 bytes; the bazel work dir is longer.
    let socket = Path::new("/tmp").join(format!("muffin-qmp-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let injector = {
        let socket = socket.clone();
        thread::spawn(move || inject_until_exit(&socket))
    };
    let report = KernelTest::new("input_nodes", env)
        .qemu_args([
            "-device".to_owned(),
            "virtio-mouse-pci".to_owned(),
            "-device".to_owned(),
            "virtio-keyboard-pci".to_owned(),
            "-qmp".to_owned(),
            format!("unix:{},server,nowait", socket.display()),
        ])
        .run();
    injector.join().expect("qmp injector thread");
    report.assert_no_line_contains("FAIL");
    report.assert_line_contains(&format!("input-nodes: motion x value={MOTION_X}"));
    report.assert_line_contains("input-nodes: key a value=1");
    report.assert_line_contains("input-nodes: ok");
    report.assert_exit_code(0, 0);
}
