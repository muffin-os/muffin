//! Prints every event of the input devices under `/dev/input` on stdout.
//!
//! Without arguments it probes `event0`, `event1`, ... until one is missing
//! and starts one child per device with the device path as the argument.
//! A child opens its device, prints the identity of the device, and then
//! prints each batch, the events up to the next sync, as one line:
//! `event1 motion X 7, motion Y -3`.

use std::io;
use std::process::{Command, exit};

use kernel_abi::{InputEvent, InputEventKind};
use muffin_sys::{ENOENT, Errno, O_RDONLY};

fn supported_kinds(fd: i32) -> Result<Vec<InputEventKind>, Errno> {
    let mut kinds = Vec::new();
    for kind in [
        InputEventKind::Key,
        InputEventKind::Motion,
        InputEventKind::Position,
    ] {
        if !muffin_sys::input_supported_codes(fd, kind)?.is_empty() {
            kinds.push(kind);
        }
    }
    Ok(kinds)
}

fn print_identity(label: &str, fd: i32) -> Result<(), Errno> {
    let info = muffin_sys::input_device_info(fd)?;
    let name = info.name().unwrap_or("<name is not UTF-8>");
    let kinds = supported_kinds(fd)?;
    println!(
        "evtest: {label} {name:?} bus=0x{:04x} vendor=0x{:04x} product=0x{:04x} version=0x{:04x} kinds={kinds:?}",
        info.bustype, info.vendor, info.product, info.version
    );
    Ok(())
}

/// Reads `path` until the read fails. Every sync event ends a batch; an empty
/// batch prints nothing.
fn watch(path: &str) -> Result<(), Errno> {
    let label = path.rsplit('/').next().unwrap_or(path);
    let fd = muffin_sys::open(path, O_RDONLY)?;
    print_identity(label, fd)?;
    let mut buf = [0u8; InputEvent::SIZE * 16];
    let mut batch: Vec<String> = Vec::new();
    loop {
        let n = muffin_sys::read(fd, &mut buf)?;
        for raw in buf[..n].as_chunks::<{ InputEvent::SIZE }>().0 {
            let event = InputEvent::from_bytes(*raw);
            if event.is_sync() {
                if !batch.is_empty() {
                    println!("{label} {}", batch.join(", "));
                    batch.clear();
                }
            } else {
                batch.push(event.to_string());
            }
        }
    }
}

fn watch_all() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut children = Vec::new();
    // devfs cannot list a directory, so a probe open is the only way to count
    // the nodes. Device nodes never go away, so the child's open cannot fail
    // where the probe succeeded.
    for index in 0.. {
        let path = format!("/dev/input/event{index}");
        match muffin_sys::open(&path, O_RDONLY) {
            Ok(fd) => muffin_sys::close(fd).map_err(|e| io::Error::other(format!("{e:?}")))?,
            Err(ENOENT) => break,
            Err(e) => return Err(io::Error::other(format!("open {path}: {e:?}"))),
        }
        children.push(Command::new(&exe).arg(&path).spawn()?);
    }
    if children.is_empty() {
        println!("evtest: no input devices");
        return Ok(());
    }
    for mut child in children {
        child.wait()?;
    }
    Ok(())
}

fn main() {
    let result = match std::env::args().nth(1) {
        Some(path) => watch(&path).map_err(|e| io::Error::other(format!("{path}: {e:?}"))),
        None => watch_all(),
    };
    if let Err(e) = result {
        println!("evtest: FAIL {e}");
        exit(1);
    }
}
