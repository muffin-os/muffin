//! This module is a client of the QEMU machine protocol. Tests use it to
//! inject input while the guest runs.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// This is the time between two injected events.
pub const INJECT_INTERVAL: Duration = Duration::from_millis(50);

/// Returns a QMP socket path in `/tmp`. A unix socket path has a limit of
/// 107 bytes, and the bazel work directory is longer. Removes an old file at
/// the path.
pub fn socket_path(test: &str) -> PathBuf {
    let socket = Path::new("/tmp").join(format!("muffin-qmp-{test}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    socket
}

/// Returns the `-qmp` argument that makes QEMU serve `socket`.
pub fn qemu_args(socket: &Path) -> [String; 2] {
    [
        "-qmp".to_owned(),
        format!("unix:{},server,nowait", socket.display()),
    ]
}

pub struct Qmp {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Qmp {
    /// Connects after QEMU makes the socket, and then does the handshake.
    pub fn connect(socket: &Path) -> Self {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let stream = loop {
            match UnixStream::connect(socket) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
                Err(e) => panic!("qmp socket did not appear: {e}"),
            }
        };
        let reader = BufReader::new(stream.try_clone().expect("clone qmp stream"));
        let mut qmp = Self { stream, reader };
        let greeting = qmp.read_line().expect("qmp greeting");
        assert!(
            greeting.contains("QMP"),
            "unexpected qmp greeting: {greeting}"
        );
        let reply = qmp
            .send("{\"execute\":\"qmp_capabilities\"}")
            .expect("qmp capabilities reply");
        assert!(
            reply.contains("\"return\""),
            "unexpected qmp reply: {reply}"
        );
        qmp
    }

    /// Sends one command line and returns the reply. Returns `None` after QEMU
    /// closes the socket.
    pub fn send(&mut self, command: &str) -> Option<String> {
        self.stream.write_all(command.as_bytes()).ok()?;
        self.stream.write_all(b"\n").ok()?;
        self.read_line()
    }

    fn read_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }
}

/// Returns an `input-send-event` command for one event. An example is
/// `input_event(r#"{"type":"rel","data":{"axis":"x","value":5}}"#)`.
pub fn input_event(event: &str) -> String {
    format!("{{\"execute\":\"input-send-event\",\"arguments\":{{\"events\":[{event}]}}}}")
}

/// Returns a relative mouse movement on `axis`, which is `x` or `y`.
pub fn mouse_motion(axis: char, value: i32) -> String {
    input_event(&format!(
        "{{\"type\":\"rel\",\"data\":{{\"axis\":\"{axis}\",\"value\":{value}}}}}"
    ))
}

/// Returns a press or a release of `button`, which is `left`, `right` or
/// `middle`.
pub fn mouse_button(button: &str, is_down: bool) -> String {
    input_event(&format!(
        "{{\"type\":\"btn\",\"data\":{{\"down\":{is_down},\"button\":\"{button}\"}}}}"
    ))
}

/// Returns a press of the key with the QEMU code `qcode`, for example `a`.
pub fn key_press(qcode: &str) -> String {
    input_event(&format!(
        "{{\"type\":\"key\",\"data\":{{\"down\":true,\"key\":{{\"type\":\"qcode\",\"data\":\"{qcode}\"}}}}}}"
    ))
}

/// Sends `commands` in sequence again and again until QEMU closes the
/// socket. QEMU sends each event to the input handler that the guest
/// activated last. Thus the virtio devices get events only after the guest
/// driver starts, and one send is not sufficient.
pub fn inject_until_exit(socket: &Path, commands: &[String]) {
    let mut qmp = Qmp::connect(socket);
    loop {
        for command in commands {
            if qmp.send(command).is_none() {
                return;
            }
            thread::sleep(INJECT_INTERVAL);
        }
    }
}
