use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::exit;
use std::thread::sleep;
use std::time::Duration;

use compositor_protocol::{
    NewWindowRequest, NewWindowResponse, Request, Response, SOCKET_PATH, read_message,
    write_message,
};

const CONNECT_ATTEMPTS: u32 = 50;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Retries because `services` starts the demo and the compositor in no defined order.
fn connect() -> io::Result<UnixStream> {
    let mut attempt = 1;
    loop {
        match UnixStream::connect(SOCKET_PATH) {
            Ok(stream) => return Ok(stream),
            Err(e) if attempt == CONNECT_ATTEMPTS => return Err(e),
            Err(_) => {
                attempt += 1;
                sleep(CONNECT_RETRY_DELAY);
            }
        }
    }
}

fn run() -> io::Result<()> {
    let stream = connect()?;
    println!("compositor-demo: connected");
    write_message(
        &mut &stream,
        &Request::NewWindow(NewWindowRequest {
            path: PathBuf::from("/var/hello.txt"),
        }),
    )?;
    let Some(response) = read_message::<Response>(&mut &stream)? else {
        return Err(io::Error::other("compositor closed the connection"));
    };
    match response {
        Response::NewWindow(NewWindowResponse {}) => println!("compositor-demo: window created"),
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        println!("compositor-demo: FAIL {e}");
        exit(1);
    }
}
