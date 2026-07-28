//! Host-side harness for booting userspace test executables inside the real
//! kernel under QEMU and asserting on their serial output and exit codes.
//!
//! A test builds a [`KernelTest`], stages files onto a fresh ext2 disk image
//! (including a `/spawn` manifest of absolute paths), boots the shared
//! `test-kernel` under QEMU, and collects the serial transcript. The kernel
//! spawns every manifest entry as its own process, so a single boot can fan
//! out into several processes whose outcomes the test kernel prints directly.
//!
//! The wire contract with `test-kernel` is four serial lines the test kernel
//! prints itself, so it is independent of the active log filter. Spawns are
//! announced by `test-kernel: spawned {path} pid={N}` and closed by
//! `test-kernel: spawn complete count={N}`. Process termination surfaces as
//! `test-kernel: outcome pid={N} exit={code}` or
//! `test-kernel: outcome pid={N} signal={NAME}`. Every parser below tolerates a
//! leading log prefix (timestamp, level, target) by anchoring on a substring
//! inside the payload rather than matching from the start.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use crate::{DiskFile, build_disk_image, build_iso};

/// Overall wall-clock budget for a single boot. QEMU under TCG in CI is slow.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(180);
/// Extra window to keep draining serial output after completion is detected.
const FINAL_MARKER_GRACE: Duration = Duration::from_secs(2);

/// Compile-time environment captured at the root test crate's call site.
///
/// `tests/support` cannot `env!` the artifact and firmware paths itself because
/// they are compile-time env of the root `muffinos` package only. The
/// [`host_env!`] macro expands where those vars exist and hands the values here.
pub struct HostEnv {
    pub ovmf_code: PathBuf,
    pub ovmf_vars: PathBuf,
    pub limine_dir: PathBuf,
    pub limine_conf: PathBuf,
    pub test_kernel: PathBuf,
    pub out_root: PathBuf,
}

/// Builds a [`HostEnv`] from the root test crate's compile-time environment.
///
/// This must expand at the caller's site, which is the only crate that sees the
/// `OVMF_*`, `LIMINE_DIR`, and `CARGO_BIN_FILE_TEST_KERNEL_test-kernel` vars set
/// by the root `build.rs` and artifact dependency.
#[macro_export]
macro_rules! host_env {
    () => {
        $crate::HostEnv {
            ovmf_code: ::std::path::PathBuf::from(env!("OVMF_X86_64_CODE")),
            ovmf_vars: ::std::path::PathBuf::from(env!("OVMF_X86_64_VARS")),
            limine_dir: ::std::path::PathBuf::from(env!("LIMINE_DIR")),
            limine_conf: ::std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/limine.conf"
            )),
            test_kernel: ::std::path::PathBuf::from(env!("CARGO_BIN_FILE_TEST_KERNEL_test-kernel")),
            out_root: ::std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/target/test-images"
            )),
        }
    };
}

/// Reports whether `qemu-system-x86_64` is on `PATH` so tests can skip on hosts
/// without QEMU (local runs) while CI, which installs it, still exercises them.
#[must_use]
pub fn qemu_available() -> bool {
    Command::new("qemu-system-x86_64")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// A configured but not-yet-booted kernel test.
pub struct KernelTest {
    name: &'static str,
    env: HostEnv,
    files: Vec<DiskFile>,
    spawn: Vec<String>,
    deadline: Duration,
    qemu_args: Vec<String>,
}

impl KernelTest {
    #[must_use]
    pub fn new(name: &'static str, env: HostEnv) -> Self {
        Self {
            name,
            env,
            files: vec![],
            spawn: vec![],
            deadline: DEFAULT_DEADLINE,
            qemu_args: vec![],
        }
    }

    /// Stages raw bytes at a disk-relative path (e.g. `"data/hello.txt"`).
    #[must_use]
    pub fn file(mut self, disk_path: &'static str, content: Vec<u8>) -> Self {
        self.files.push(DiskFile {
            path: disk_path,
            content,
        });
        self
    }

    /// Stages a prebuilt host binary at a disk-relative path (e.g. `"bin/mmap"`).
    #[must_use]
    pub fn program(self, disk_path: &'static str, host_binary: impl AsRef<Path>) -> Self {
        let content =
            fs::read(host_binary.as_ref()).expect("should be able to read the test program binary");
        self.file(disk_path, content)
    }

    /// Appends one absolute in-OS path to the `/spawn` manifest. Spawning the
    /// same path twice is allowed and is how a test fans out into multiple
    /// processes.
    #[must_use]
    pub fn spawn(mut self, abs_path: &str) -> Self {
        self.spawn.push(abs_path.to_owned());
        self
    }

    #[must_use]
    pub fn deadline(mut self, d: Duration) -> Self {
        self.deadline = d;
        self
    }

    /// Appends extra QEMU arguments to the hardcoded baseline, for example a
    /// virtio-gpu device pinned to a fixed resolution.
    #[must_use]
    pub fn qemu_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.qemu_args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Boots the test kernel under QEMU and returns the collected transcript
    /// and per-process outcomes.
    ///
    /// # Panics
    /// Panics (after dumping the serial transcript) if no spawn entries were
    /// configured, if QEMU exits before every spawned process reports an
    /// outcome, if the deadline expires first, or if any serial line contains
    /// `panicked`.
    #[must_use]
    pub fn run(mut self) -> RunReport {
        assert!(
            !self.spawn.is_empty(),
            "KernelTest::run requires at least one spawn entry"
        );

        // The manifest is a plain newline-separated list of absolute paths the
        // kernel reads from `/spawn`. A trailing newline keeps every entry on
        // its own line.
        let manifest = format!("{}\n", self.spawn.join("\n"));
        self.files.push(DiskFile {
            path: "spawn",
            content: manifest.into_bytes(),
        });

        let out_dir = self.env.out_root.join(self.name);
        fs::create_dir_all(&out_dir).expect("should be able to create test image output directory");
        let disk_image = build_disk_image(&self.files, &out_dir);
        let bootable_iso = build_iso(
            &self.env.limine_dir,
            &self.env.limine_conf,
            &self.env.test_kernel,
            &out_dir,
        );

        let ovmf_code = self.env.ovmf_code.display();
        let ovmf_vars = self.env.ovmf_vars.display();
        // Accelerate with KVM when the host exposes it, falling back to TCG so
        // the test still runs on CI hosts without /dev/kvm. `-cpu max` (below)
        // stays valid under both, unlike `-cpu host`, which requires a hardware
        // accelerator.
        let accel: &[&str] = if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
            &["-machine", "accel=kvm:tcg"]
        } else {
            &[]
        };
        let mut child = Command::new("qemu-system-x86_64")
            .arg("-serial")
            .arg("stdio")
            .arg("-display")
            .arg("none")
            // The runner's default. 512M shrinks the kernel heap enough to OOM
            // during boot (backtrace context initialization).
            .arg("-m")
            .arg("4G")
            .arg("-drive")
            .arg(format!(
                "if=pflash,unit=0,format=raw,file={ovmf_code},readonly=on"
            ))
            .arg("-drive")
            .arg(format!("if=pflash,unit=1,format=raw,file={ovmf_vars}"))
            .arg("-cdrom")
            .arg(&bootable_iso)
            .arg("-cpu")
            .arg("max")
            .arg("-smp")
            .arg("4")
            .args(accel)
            .arg("-drive")
            .arg(format!(
                "id=virtio-disk0,file={},format=raw,if=none",
                disk_image.display()
            ))
            .arg("-device")
            .arg("virtio-blk-pci,drive=virtio-disk0")
            .args(&self.qemu_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn qemu-system-x86_64");

        let stdout = child.stdout.take().expect("child stdout was not captured");

        let (tx, rx) = mpsc::channel::<String>();
        let reader = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut transcript: Vec<String> = vec![];
        let mut pending: Vec<PendingProcess> = vec![];
        let mut all_announced = false;
        let deadline = Instant::now() + self.deadline;
        let mut complete_at: Option<Instant> = None;
        let mut child_exited_early = false;

        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            // Once completion is detected, only wait out the grace window so any
            // trailing lines still land in the transcript.
            let timeout = match complete_at {
                Some(seen) => {
                    let grace_end = seen + FINAL_MARKER_GRACE;
                    if now >= grace_end {
                        break;
                    }
                    (grace_end - now).min(deadline - now)
                }
                None => deadline - now,
            };
            match rx.recv_timeout(timeout) {
                Ok(line) => {
                    if line.contains("panicked") {
                        transcript.push(line.clone());
                        fail(
                            &transcript,
                            &mut child,
                            format!("kernel panicked during the run: {line:?}"),
                        );
                    }
                    if let Some((path, pid)) = parse_spawned(&line) {
                        pending.push(PendingProcess {
                            path,
                            pid,
                            outcome: None,
                        });
                    } else if parse_spawn_complete(&line).is_some() {
                        all_announced = true;
                    } else if let Some((pid, code)) = parse_exit(&line)
                        && let Some(proc) = pending.iter_mut().find(|p| p.pid == pid)
                    {
                        proc.outcome = Some(Outcome::Exited(code));
                    } else if let Some((name, pid)) = parse_signal(&line)
                        && let Some(proc) = pending.iter_mut().find(|p| p.pid == pid)
                    {
                        proc.outcome = Some(Outcome::Signaled(name));
                    }
                    transcript.push(line);

                    // Completion requires every announced process to have
                    // terminated, so the transcript is guaranteed to carry all
                    // outcomes before we stop reading.
                    if complete_at.is_none()
                        && all_announced
                        && pending.iter().all(|p| p.outcome.is_some())
                    {
                        complete_at = Some(Instant::now());
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    // stdout closed: QEMU exited on its own.
                    child_exited_early = complete_at.is_none();
                    break;
                }
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();

        if child_exited_early {
            fail(
                &transcript,
                &mut child,
                "QEMU exited before every spawned process reported an outcome; kernel likely \
                 panicked or triple-faulted"
                    .to_owned(),
            );
        }
        if complete_at.is_none() {
            fail(
                &transcript,
                &mut child,
                "deadline expired before every spawned process reported an outcome".to_owned(),
            );
        }

        let processes = pending
            .into_iter()
            .map(|p| SpawnedProcess {
                path: p.path,
                pid: p.pid,
                outcome: p
                    .outcome
                    .expect("completion guarantees every process has an outcome"),
            })
            .collect();

        RunReport {
            transcript,
            processes,
        }
    }
}

/// A spawned process whose outcome is still being awaited during the run.
struct PendingProcess {
    path: String,
    pid: u64,
    outcome: Option<Outcome>,
}

/// How a spawned process ended.
pub enum Outcome {
    Exited(u64),
    Signaled(String),
}

/// A process the kernel spawned from the manifest, with its final outcome.
pub struct SpawnedProcess {
    pub path: String,
    pub pid: u64,
    pub outcome: Outcome,
}

/// The result of a completed boot: the full serial transcript and the outcome
/// of every spawned process in manifest (and thus pid) order.
pub struct RunReport {
    pub transcript: Vec<String>,
    pub processes: Vec<SpawnedProcess>,
}

impl RunReport {
    /// Exit code of the `index`-th spawned process, or `None` if it was
    /// signaled rather than exiting.
    #[must_use]
    pub fn exit_code(&self, index: usize) -> Option<u64> {
        match self.processes.get(index).map(|p| &p.outcome) {
            Some(Outcome::Exited(code)) => Some(*code),
            _ => None,
        }
    }

    /// Asserts the `index`-th spawned process exited with `expected`.
    ///
    /// # Panics
    /// Dumps the transcript and panics if the process is missing, was signaled,
    /// or exited with a different code.
    pub fn assert_exit_code(&self, index: usize, expected: u64) {
        match self.exit_code(index) {
            Some(code) if code == expected => {}
            other => {
                self.dump();
                panic!("process index {index} exit code was {other:?}, expected Some({expected})");
            }
        }
    }

    /// Asserts every marker appears in the transcript in order, each at or after
    /// the previous one's line.
    ///
    /// # Panics
    /// Dumps the transcript and panics naming the first marker index not found
    /// at or after its predecessor.
    pub fn assert_markers_in_order(&self, markers: &[&str]) {
        let mut search_from = 0usize;
        for (i, marker) in markers.iter().enumerate() {
            match self.transcript[search_from..]
                .iter()
                .position(|line| line.contains(marker))
            {
                Some(offset) => search_from += offset,
                None => {
                    self.dump();
                    panic!("marker {i} ({marker:?}) not found at or after the previous marker");
                }
            }
        }
    }

    /// Asserts some transcript line contains `needle`.
    ///
    /// # Panics
    /// Dumps the transcript and panics if no line contains `needle`.
    pub fn assert_line_contains(&self, needle: &str) {
        if !self.transcript.iter().any(|line| line.contains(needle)) {
            self.dump();
            panic!("no transcript line contained {needle:?}");
        }
    }

    /// Asserts no transcript line contains `needle`.
    ///
    /// # Panics
    /// Dumps the transcript and panics if any line contains `needle`.
    pub fn assert_no_line_contains(&self, needle: &str) {
        if let Some(line) = self.transcript.iter().find(|line| line.contains(needle)) {
            self.dump();
            panic!("found forbidden line containing {needle:?}: {line:?}");
        }
    }

    fn dump(&self) {
        dump_transcript(&self.transcript);
    }
}

fn dump_transcript(transcript: &[String]) {
    eprintln!(
        "===== QEMU serial transcript ({} lines) =====",
        transcript.len()
    );
    for line in transcript {
        eprintln!("{line}");
    }
    eprintln!("===== end transcript =====");
}

/// Dumps the transcript, kills the child, then fails the test.
fn fail(transcript: &[String], child: &mut Child, message: String) -> ! {
    dump_transcript(transcript);
    let _ = child.kill();
    let _ = child.wait();
    panic!("{message}");
}

/// Parses `test-kernel: spawned {path} pid={N}` into `(path, pid)`.
fn parse_spawned(line: &str) -> Option<(String, u64)> {
    let rest = anchor_after(line, "test-kernel: spawned ")?;
    let (path, pid) = rest.rsplit_once(" pid=")?;
    Some((path.trim().to_owned(), pid.trim().parse().ok()?))
}

/// Parses `test-kernel: spawn complete count={N}` into `N`.
fn parse_spawn_complete(line: &str) -> Option<u64> {
    anchor_after(line, "test-kernel: spawn complete count=")?
        .trim()
        .parse()
        .ok()
}

/// Parses `test-kernel: outcome pid={N} exit={code}` into `(pid, code)`.
fn parse_exit(line: &str) -> Option<(u64, u64)> {
    let rest = anchor_after(line, "test-kernel: outcome pid=")?;
    let (pid, code) = rest.split_once(" exit=")?;
    Some((pid.trim().parse().ok()?, code.trim().parse().ok()?))
}

/// Parses `test-kernel: outcome pid={N} signal={NAME}` into `(name, pid)`.
fn parse_signal(line: &str) -> Option<(String, u64)> {
    let rest = anchor_after(line, "test-kernel: outcome pid=")?;
    let (pid, name) = rest.split_once(" signal=")?;
    Some((name.trim().to_owned(), pid.trim().parse().ok()?))
}

/// Returns the text following `anchor`, or `None` if `anchor` is absent. Used to
/// skip any log prefix (timestamp, level, target) before a known payload.
fn anchor_after<'a>(line: &'a str, anchor: &str) -> Option<&'a str> {
    line.find(anchor).map(|idx| &line[idx + anchor.len()..])
}
