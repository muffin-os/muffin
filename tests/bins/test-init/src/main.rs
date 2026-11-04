//! The init process of the integration tests. It lists `/bin/tests`, sorts the
//! names byte-wise, spawns one child per file and reaps every child. It prints
//! the wire-contract lines the host harness parses: `spawned`, `spawn complete`
//! and `outcome`.

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use muffin_sys::Signal;

fn main() -> ExitCode {
    let dir = Path::new("/bin/tests");
    let names = fs::read_dir(dir).and_then(|entries| {
        entries
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
    });
    let mut names = match names {
        Ok(names) => names,
        Err(e) => {
            println!("test-init: FAIL read_dir /bin/tests {e}");
            return ExitCode::FAILURE;
        }
    };
    // The assertions index the spawn order, so it must not depend on the disk order.
    names.sort();

    let mut children = Vec::new();
    for name in names {
        let path = dir.join(name);
        // muffin's `spawn` has no exec error pipe: a failed exec shows up as `exit=127`.
        match Command::new(&path).env_clear().spawn() {
            Ok(child) => {
                println!("test-init: spawned {} pid={}", path.display(), child.id());
                children.push(child);
            }
            Err(e) => {
                println!("test-init: FAIL spawn {} {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    println!("test-init: spawn complete count={}", children.len());

    for mut child in children {
        let pid = child.id();
        let status = match child.wait() {
            Ok(status) => status,
            Err(e) => {
                println!("test-init: FAIL waitpid {pid} {e}");
                return ExitCode::FAILURE;
            }
        };
        match status.signal().and_then(|signo| Signal::try_from(signo).ok()) {
            Some(signal) => println!("test-init: outcome pid={pid} signal={}", signal.name()),
            None => println!("test-init: outcome pid={pid} exit={}", status.into_raw()),
        }
    }
    ExitCode::SUCCESS
}
