#![no_std]
#![no_main]

//! The init process of the integration tests. It lists `/bin/tests`, sorts the
//! names byte-wise, forks one child per file and execs the file in the child.
//! Then it reaps every child and prints the wire-contract lines the host
//! harness parses: `spawned`, `spawn complete` and `outcome`.

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::{self, Write};

use minilib::{Signal, Stdout, execve, exit, fork, println, read_dir, waitpid};

minilib::entry!(main);

/// The children print concurrently and the serial port is shared, so a
/// wire-contract line goes out in one write syscall, which holds the serial
/// lock for the whole line.
fn report(line: fmt::Arguments) {
    let _ = Stdout.write_str(&alloc::fmt::format(line));
}

fn main() -> i32 {
    let mut names = match read_dir("/bin/tests") {
        Ok(names) => names,
        Err(e) => {
            println!("test-init: FAIL read_dir /bin/tests {e}");
            return 1;
        }
    };
    names.retain(|name| name != "." && name != "..");
    // The assertions index the spawn order, so it must not depend on the disk order.
    names.sort();

    let mut children = Vec::new();
    for name in names {
        let path = alloc::format!("/bin/tests/{name}");
        let path = path.as_str();
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
