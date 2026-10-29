#![no_std]
#![no_main]

//! The init process of the integration tests. It reads `/spawn`, one absolute
//! path per line, forks one child per line and execs the path in the child.
//! Then it reaps every child and prints the wire-contract lines the host
//! harness parses: `spawned`, `spawn complete` and `outcome`.

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

use minilib::{O_RDONLY, Signal, close, execve, exit, fork, open, println, read, waitpid, write};

minilib::entry!(main);

/// The children inherit every open fd across fork and exec, so the manifest
/// fd is closed before the first fork.
fn read_manifest() -> Result<Vec<u8>, &'static str> {
    let fd = open("/spawn", O_RDONLY).map_err(|_| "open")?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let n = read(fd, &mut chunk).map_err(|_| "read")?;
        if n == 0 {
            close(fd).map_err(|_| "close")?;
            return Ok(buf);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// The children print concurrently and the serial port is shared, so a
/// wire-contract line goes out in one write syscall, which holds the serial
/// lock for the whole line.
fn report(line: fmt::Arguments) {
    let line = alloc::fmt::format(line);
    let mut rest = line.as_bytes();
    while let Ok(n) = write(1, rest)
        && n > 0
    {
        rest = &rest[n..];
    }
}

fn main() -> i32 {
    let manifest = match read_manifest() {
        Ok(manifest) => manifest,
        Err(what) => {
            println!("test-init: FAIL {what} /spawn");
            return 1;
        }
    };
    let Ok(manifest) = core::str::from_utf8(&manifest) else {
        println!("test-init: FAIL /spawn is not UTF-8");
        return 1;
    };

    let mut children = Vec::new();
    for path in manifest.lines().filter(|line| !line.is_empty()) {
        match fork() {
            Ok(0) => {
                let errno = execve(path, &[path], &[]);
                println!("test-init: FAIL exec {path} {errno}");
                exit(1);
            }
            Ok(pid) => {
                report(format_args!("test-init: spawned {path} pid={pid}\n"));
                children.push(pid);
            }
            Err(e) => {
                println!("test-init: FAIL fork {e}");
                return 1;
            }
        }
    }
    report(format_args!(
        "test-init: spawn complete count={}\n",
        children.len()
    ));

    for pid in children {
        let status = match waitpid(pid) {
            Ok(status) => status,
            Err(e) => {
                println!("test-init: FAIL waitpid {pid} {e}");
                return 1;
            }
        };
        // shortcut: an exit code of 128 or more reads as a signal, no test exits that high
        match Signal::try_from(status - 128) {
            Ok(signal) if status >= 128 => report(format_args!(
                "test-init: outcome pid={pid} signal={}\n",
                signal.name()
            )),
            _ => report(format_args!("test-init: outcome pid={pid} exit={status}\n")),
        }
    }
    0
}
