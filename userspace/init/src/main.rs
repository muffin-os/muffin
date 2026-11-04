use std::io;
use std::process::{Command, ExitCode};

const SERVICE_MANAGER: &str = "/bin/services";

// std has no wait-any. `Child::wait` is `waitpid(child_pid)`, so orphans
// need the POSIX `waitpid(-1)` from the libc std already links.
unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

fn main() -> ExitCode {
    let manager = match Command::new(SERVICE_MANAGER)
        .env("RUST_BACKTRACE", "1")
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("init: cannot start {SERVICE_MANAGER}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let manager_pid = manager.id() as i32;

    loop {
        let mut status = 0;
        let pid = unsafe { waitpid(-1, &raw mut status, 0) };
        if pid == -1 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // ECHILD: no child is left, so the manager is gone too.
            eprintln!("init: waitpid failed: {e}");
            return ExitCode::FAILURE;
        }
        if pid == manager_pid {
            eprintln!("init: {SERVICE_MANAGER} exited with status {status:#x}");
            return ExitCode::FAILURE;
        }
    }
}
