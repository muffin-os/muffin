#![no_std]
#![no_main]

//! The init process of the integration tests. It lists `/bin/tests`, sorts the
//! names byte-wise, forks one child per file and execs the file in the child.
//! Then it reaps every child and prints the wire-contract lines the host
//! harness parses: `spawned`, `spawn complete` and `outcome`.

extern crate alloc;

use alloc::vec::Vec;

use minilib::{Signal, execve, exit, fork, println, read_dir, waitpid};

minilib::entry!(main);

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
                println!("test-init: spawned {path} pid={pid}");
                children.push(pid);
            }
            Err(e) => {
                println!("test-init: FAIL fork {e}");
                return 1;
            }
        }
    }
    println!("test-init: spawn complete count={}", children.len());

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
            Ok(signal) if status >= 128 => {
                println!("test-init: outcome pid={pid} signal={}", signal.name())
            }
            _ => println!("test-init: outcome pid={pid} exit={status}"),
        }
    }
    0
}
