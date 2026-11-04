//! Starts `/bin/compositor` as a detached child and runs `/bin/compositor-demo`
//! to completion. test-init waits for this binary only, so the compositor may
//! keep running until the poweroff.

use std::io;
use std::process::{Command, exit};

fn main() {
    if let Err(e) = run() {
        println!("compositor-smoke: FAIL {e}");
        exit(1);
    }
}

fn run() -> io::Result<()> {
    Command::new("/bin/compositor").spawn()?;
    let status = Command::new("/bin/compositor-demo").status()?;
    if status.code() != Some(0) {
        return Err(io::Error::other(format!(
            "compositor-demo exited with {status}"
        )));
    }
    println!("compositor-smoke: demo exit=0");
    Ok(())
}
