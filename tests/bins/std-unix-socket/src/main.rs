//! Userspace exerciser for `std::os::unix::net` on muffin.
//!
//! Without arguments, the binary is the server. It listens and spawns itself
//! as `client`. The server returns `ROUND_TRIPS` numbered messages to the
//! client. Then the two sides exchange one line in each direction. Each step
//! prints `std-unix-socket: <step>`. An error prints
//! `std-unix-socket: FAIL <error>` and exits with code 1.

use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, exit};

const PATH: &str = "/std-unix-socket";
/// Enough blocking round trips to hit a wakeup that races the peer's write.
const ROUND_TRIPS: u32 = 20_000;

fn ping(stream: &mut UnixStream) -> Result<(), Box<dyn Error>> {
    for n in 0..ROUND_TRIPS {
        stream.write_all(&n.to_le_bytes())?;
        let mut reply = [0; 4];
        stream.read_exact(&mut reply)?;
        let got = u32::from_le_bytes(reply);
        if got != n {
            return Err(format!("round trip {n} got {got}").into());
        }
    }
    Ok(())
}

fn pong(stream: &UnixStream) -> Result<(), Box<dyn Error>> {
    for _ in 0..ROUND_TRIPS {
        let mut message = [0; 4];
        (&*stream).read_exact(&mut message)?;
        (&*stream).write_all(&message)?;
    }
    Ok(())
}

fn main() {
    let result = if std::env::args().nth(1).as_deref() == Some("client") {
        client()
    } else {
        server()
    };
    if let Err(e) = result {
        println!("std-unix-socket: FAIL {e}");
        exit(1);
    }
}

fn client() -> Result<(), Box<dyn Error>> {
    let mut stream = UnixStream::connect(PATH)?;
    println!("std-unix-socket: client connected");
    ping(&mut stream)?;
    println!("std-unix-socket: client {ROUND_TRIPS} round trips ok");
    stream.write_all(b"hello from client\n")?;
    stream.shutdown(Shutdown::Write)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    if reply != "hello from server\n" {
        return Err(format!("client got {reply:?}").into());
    }
    println!("std-unix-socket: client got hello from server");
    Ok(())
}

fn server() -> Result<(), Box<dyn Error>> {
    let listener = UnixListener::bind(PATH)?;
    println!("std-unix-socket: server listening");
    let mut child = Command::new(std::env::current_exe()?)
        .arg("client")
        .spawn()?;
    let (stream, addr) = listener.accept()?;
    if !addr.is_unnamed() {
        return Err(format!("peer address {addr:?} is not unnamed").into());
    }
    pong(&stream)?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    if line != "hello from client\n" {
        return Err(format!("server got {line:?}").into());
    }
    println!("std-unix-socket: server got hello from client");
    (&stream).write_all(b"hello from server\n")?;
    drop(stream);
    let status = child.wait()?;
    if status.code() != Some(0) {
        return Err(format!("client exited with {status}").into());
    }
    println!("std-unix-socket: server child exit=0");
    Ok(())
}
