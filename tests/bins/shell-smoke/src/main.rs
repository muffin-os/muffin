//! Starts the compositor with `--probe` and the shell. The host types into
//! the shell, and then the `poweroff` command of the shell powers the
//! machine off. Thus the shell must not end.

use std::io;
use std::process::{Command, exit};

fn main() {
    if let Err(e) = run() {
        println!("shell-smoke: FAIL {e}");
        exit(1);
    }
}

fn run() -> io::Result<()> {
    Command::new("/bin/compositor").arg("--probe").spawn()?;
    let status = Command::new("/bin/shell").spawn()?.wait()?;
    Err(io::Error::other(format!("the shell ended, {status}")))
}
