use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::exit;
use std::thread::sleep;
use std::time::Duration;

use compositor_protocol::{
    NewWindowRequest, NewWindowResponse, Request, Response, SOCKET_PATH, read_message,
    write_message,
};
use muffin_sys::{MapFlags, ProtFlags, mmap, munmap};

const CONNECT_ATTEMPTS: u32 = 50;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);
const WIDTH: u32 = 256;
const HEIGHT: u32 = 128;
const PIXEL: u32 = 0xFF20_60C0;
const SHM_PATH: &str = "/dev/shm/compositor-demo";

/// Retries because `services` spawns the compositor first but does not wait for its socket.
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

    let request = NewWindowRequest {
        path: SHM_PATH.into(),
        width: WIDTH,
        height: HEIGHT,
    };
    let len = request
        .buffer_len()
        .ok_or_else(|| io::Error::other("size"))?;
    // `truncate` keeps a rerun working when an earlier run left the object.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(SHM_PATH)?;
    file.set_len(len as u64)?;
    let addr = mmap(
        0,
        len,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        file.as_raw_fd(),
        0,
    )
    .map_err(|_| io::Error::other("mmap"))?;
    // SAFETY: the mapping is writable, covers `len` bytes and is page-aligned,
    // hence u32-aligned. `pixels` is not used after the `munmap` below.
    let pixels =
        unsafe { std::slice::from_raw_parts_mut(addr.cast::<u32>(), len / size_of::<u32>()) };
    pixels.fill(PIXEL);

    write_message(&mut &stream, &Request::NewWindow(request))?;
    let Some(response) = read_message::<Response>(&mut &stream)? else {
        return Err(io::Error::other("compositor closed the connection"));
    };
    unsafe { munmap(addr, len) }.map_err(|_| io::Error::other("munmap"))?;
    drop(file);
    fs::remove_file(SHM_PATH)?;
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
