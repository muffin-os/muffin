use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, io, thread};

use serde::Deserialize;

const SERVICE_DIR: &str = "/etc/services";

/// Keeps a service that fails at once from spinning the manager.
const RESTART_DELAY: Duration = Duration::from_secs(1);

// std has no wait-any. `Child::wait` is `waitpid(child_pid)`, so reaping
// whichever service exits first needs the POSIX `waitpid(-1)` from the libc
// std already links.
unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

#[derive(Deserialize)]
struct Service {
    exec: PathBuf,
    #[serde(default)]
    restart: Restart,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Restart {
    #[default]
    No,
    OnFailure,
    Always,
}

impl Restart {
    /// `status` is the kernel's wait status: the exit code, or 128 plus the
    /// signal number.
    fn applies_to(self, status: i32) -> bool {
        match self {
            Restart::No => false,
            Restart::OnFailure => status != 0,
            Restart::Always => true,
        }
    }
}

fn main() -> ExitCode {
    let services = match load_services(Path::new(SERVICE_DIR)) {
        Ok(services) => services,
        Err(e) => {
            println!("services: FAIL {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("services: parsed {} services", services.len());

    // pid -> service name
    let mut running = BTreeMap::new();
    for (name, service) in &services {
        spawn(name, service, &mut running);
    }

    loop {
        let mut status = 0;
        let pid = unsafe { waitpid(-1, &raw mut status, 0) };
        if pid == -1 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // ECHILD: no service is left to restart. init treats an exit of
            // the manager as fatal, so stay alive.
            println!("services: idle: {e}");
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        }
        let Some(name) = running.remove(&pid) else {
            continue;
        };
        println!("services: {name} pid={pid} exited status={status}");
        let service = &services[&name];
        if service.restart.applies_to(status) {
            // shortcut: the delay blocks reaping of every other service, until the manager has a timer
            thread::sleep(RESTART_DELAY);
            println!("services: restarting {name}");
            spawn(&name, service, &mut running);
        }
    }
}

fn spawn(name: &str, service: &Service, running: &mut BTreeMap<i32, String>) {
    match Command::new(&service.exec).spawn() {
        Ok(child) => {
            let pid = child.id() as i32;
            println!("services: spawned {name} pid={pid}");
            running.insert(pid, name.to_owned());
        }
        Err(e) => println!(
            "services: failed to spawn {name} ({}): {e}",
            service.exec.display()
        ),
    }
}

/// Loads one service per regular file in `dir`. The file stem names the
/// service. A missing directory is an empty collection.
fn load_services(dir: &Path) -> Result<BTreeMap<String, Service>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("read {}: {e}", dir.display())),
    };
    let mut services = BTreeMap::new();
    for entry in entries {
        let path = entry
            .map(|e| e.path())
            .map_err(|e| format!("read {}: {e}", dir.display()))?;
        // `metadata` follows symlinks, so a link to a regular file counts.
        let metadata = fs::metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            return Err(format!("{}: file name is not UTF-8", path.display()));
        };
        let text =
            fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let service =
            rust_yaml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        services.insert(name.to_owned(), service);
    }
    Ok(services)
}
