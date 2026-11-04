use std::io;
use std::process::{Child, Command, ExitCode};
use std::time::{Duration, Instant};

const SERVICE_MANAGER: &str = "/bin/services";
const MAX_RESTARTS: usize = 2; // giving us three runs max per period
const RESTART_WINDOW: Duration = Duration::from_hours(1);

// std has no wait-any. `Child::wait` is `waitpid(child_pid)`, so orphans
// need the POSIX `waitpid(-1)` from the libc std already links.
unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

fn spawn_manager() -> Option<Child> {
    let mut command = Command::new(SERVICE_MANAGER);
    if cfg!(debug_assertions) {
        command.env("RUST_BACKTRACE", "1");
    }
    match command.spawn() {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("init: cannot start {SERVICE_MANAGER}: {e}");
            None
        }
    }
}

// Drops restarts older than the window and records this one. Returns false
// when the window already holds MAX_RESTARTS.
fn record_restart(restarts: &mut Vec<Instant>, now: Instant) -> bool {
    restarts.retain(|at| now.duration_since(*at) < RESTART_WINDOW);
    if restarts.len() >= MAX_RESTARTS {
        return false;
    }
    restarts.push(now);
    true
}

fn minutes_seconds(d: Duration) -> String {
    format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60)
}

fn main() -> ExitCode {
    let Some(manager) = spawn_manager() else {
        return ExitCode::FAILURE;
    };
    let mut manager_pid = manager.id() as i32;
    let mut restarts = Vec::new();

    loop {
        let mut status = 0;
        let pid = unsafe { waitpid(-1, &raw mut status, 0) };
        if pid == -1 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // ECHILD: no child is left
            eprintln!("init: waitpid failed: {e}");
            return ExitCode::FAILURE;
        }
        if pid == manager_pid {
            eprintln!("init: {SERVICE_MANAGER} exited with status {status:#}");
            let now = Instant::now();
            if !record_restart(&mut restarts, now) {
                eprintln!(
                    "init: {SERVICE_MANAGER} exited {MAX_RESTARTS} times within {}, giving up",
                    minutes_seconds(now - restarts[0])
                );
                return ExitCode::FAILURE;
            }
            let Some(manager) = spawn_manager() else {
                return ExitCode::FAILURE;
            };
            manager_pid = manager.id() as i32;
            // retain keeps insertion order, so the oldest restart is first.
            eprintln!(
                "init: restarted {SERVICE_MANAGER} as pid {manager_pid}, {} of {MAX_RESTARTS} restarts left within the next {}",
                MAX_RESTARTS - restarts.len(),
                minutes_seconds(restarts[0] + RESTART_WINDOW - now)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourth_restart_within_window_is_refused() {
        let start = Instant::now();
        let mut restarts = Vec::new();
        for minute in 0..MAX_RESTARTS as u64 {
            assert!(record_restart(
                &mut restarts,
                start + Duration::from_secs(60 * minute)
            ));
        }
        assert!(!record_restart(
            &mut restarts,
            start + Duration::from_secs(60 * 30)
        ));
    }

    #[test]
    fn restart_outside_window_frees_a_slot() {
        let start = Instant::now();
        let mut restarts = Vec::new();
        for _ in 0..MAX_RESTARTS {
            assert!(record_restart(&mut restarts, start));
        }
        assert!(record_restart(&mut restarts, start + RESTART_WINDOW));
        assert_eq!(restarts.len(), 1);
    }
}
