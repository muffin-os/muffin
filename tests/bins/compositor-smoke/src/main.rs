use std::io;
use std::process::{Command, exit};
use std::thread::sleep;
use std::time::Duration;

/// Long enough for the demo to print at least one frame-rate report.
const DEMO_RUN_TIME: Duration = Duration::from_secs(3);

fn main() {
    if let Err(e) = run() {
        println!("compositor-smoke: FAIL {e}");
        exit(1);
    }
}

fn run() -> io::Result<()> {
    Command::new("/bin/compositor").spawn()?;
    let mut demo = Command::new("/bin/compositor-demo").spawn()?;
    sleep(DEMO_RUN_TIME);
    demo.kill()?;
    let status = demo.wait()?;
    println!("compositor-smoke: demo killed, {status}");
    Ok(())
}
