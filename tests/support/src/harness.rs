//! Host-side harness for booting userspace test executables inside the real
//! kernel under QEMU and asserting on their serial output and exit codes.
//!
//! A test names a [`KernelTest`], boots the shared `test-kernel` under QEMU
//! against the ISO and disk its Bazel target declared, and collects the serial
//! transcript. The disk carries a `/spawn` manifest of absolute paths. The
//! kernel spawns every manifest entry as its own process, so a single boot can
//! fan out into several processes whose outcomes the test kernel prints
//! directly.
//!
//! The wire contract with `test-kernel` is four serial lines the test kernel
//! prints itself, so it is independent of the active log filter. Spawns are
//! announced by `test-kernel: spawned {path} pid={N}` and closed by
//! `test-kernel: spawn complete count={N}`. Process termination surfaces as
//! `test-kernel: outcome pid={N} exit={code}` or
//! `test-kernel: outcome pid={N} signal={NAME}`. Every parser below tolerates a
//! leading log prefix (timestamp, level, target) by anchoring on a substring
//! inside the payload rather than matching from the start.

use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{fs, thread};

/// Overall wall-clock budget for a single boot. QEMU under TCG in CI is slow.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(180);
/// Extra window to keep draining serial output after completion is detected.
const FINAL_MARKER_GRACE: Duration = Duration::from_secs(2);

/// Paths to the images and firmware a single test boots.
///
/// `tests/support` cannot `env!` these itself because `rustc_env` sets them on
/// each test crate, not here. The [`host_env!`] macro expands at the caller and
/// hands the values over.
pub struct HostEnv {
    pub iso: PathBuf,
    pub disk: PathBuf,
    pub ovmf_code: PathBuf,
    pub ovmf_vars: PathBuf,
    pub work_dir: PathBuf,
}

/// Builds a [`HostEnv`] from the calling test crate's compile-time environment.
///
/// This must expand at the caller's site, which is the only crate whose
/// `rustc_env` carries the four image and firmware paths.
#[macro_export]
macro_rules! host_env {
    () => {
        $crate::HostEnv {
            iso: ::std::path::PathBuf::from(env!("MUFFIN_TEST_ISO")),
            disk: ::std::path::PathBuf::from(env!("MUFFIN_TEST_DISK")),
            ovmf_code: ::std::path::PathBuf::from(env!("OVMF_X86_64_CODE")),
            ovmf_vars: ::std::path::PathBuf::from(env!("OVMF_X86_64_VARS")),
            work_dir: ::std::path::PathBuf::from(
                ::std::env::var("TEST_TMPDIR").expect("TEST_TMPDIR is set by bazel test"),
            ),
        }
    };
}

/// A configured but not-yet-booted kernel test.
pub struct KernelTest {
    name: &'static str,
    env: HostEnv,
    deadline: Duration,
    qemu_args: Vec<String>,
}

impl KernelTest {
    #[must_use]
    pub fn new(name: &'static str, env: HostEnv) -> Self {
        Self {
            name,
            env,
            deadline: DEFAULT_DEADLINE,
            qemu_args: vec![],
        }
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

    /// Stages the writable images and spawns QEMU. Shared by [`Self::run`] and
    /// [`Self::run_until_poweroff`].
    fn boot(&self) -> Child {
        let out_dir = self.env.work_dir.join(self.name);
        fs::create_dir_all(&out_dir).expect("should be able to create the test work directory");
        let disk_image = stage_writable(&self.env.disk, &out_dir.join("disk.img"));
        let ovmf_vars_image = stage_writable(&self.env.ovmf_vars, &out_dir.join("ovmf-vars.fd"));

        let ovmf_code = self.env.ovmf_code.display();
        let ovmf_vars = ovmf_vars_image.display();
        // Falls back to TCG so CI hosts without /dev/kvm still boot. `-cpu max`
        // below stays valid under both, unlike `-cpu host`.
        let accel: &[&str] = if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
            &["-machine", "accel=kvm:tcg"]
        } else {
            &[]
        };
        Command::new("qemu-system-x86_64")
            .arg("-serial")
            .arg("stdio")
            .arg("-display")
            .arg("none")
            // 512M OOMs the kernel heap during boot.
            .arg("-m")
            .arg("4G")
            .arg("-drive")
            .arg(format!(
                "if=pflash,unit=0,format=raw,file={ovmf_code},readonly=on"
            ))
            .arg("-drive")
            .arg(format!("if=pflash,unit=1,format=raw,file={ovmf_vars}"))
            .arg("-device")
            .arg("ahci,id=ahci0")
            .arg("-drive")
            .arg(format!(
                "if=none,id=cd0,media=cdrom,format=raw,file={}",
                self.env.iso.display()
            ))
            .arg("-device")
            .arg("ide-cd,drive=cd0,bus=ahci0.0")
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
            .expect("failed to spawn qemu-system-x86_64")
    }

    /// Boots the test kernel under QEMU and returns the collected transcript
    /// and per-process outcomes.
    ///
    /// # Panics
    /// Panics (after dumping the serial transcript) if QEMU exits before every
    /// spawned process reports an outcome, if the deadline expires first, or if
    /// any serial line contains `kernel panicked`.
    #[must_use]
    pub fn run(self) -> RunReport {
        let mut child = self.boot();
        let (rx, reader) = spawn_serial_reader(&mut child);

        let mut transcript: Vec<String> = vec![];
        let mut tracker = SpawnTracker::default();
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
                Ok(Err(e)) => fail(&transcript, &mut child, format!("serial read failed: {e}")),
                Ok(Ok(line)) => {
                    if line.contains("kernel panicked") {
                        transcript.push(line.clone());
                        fail(
                            &transcript,
                            &mut child,
                            format!("kernel panicked during the run: {line:?}"),
                        );
                    }
                    if let Err(message) = tracker.observe(&line) {
                        transcript.push(line);
                        fail(&transcript, &mut child, message);
                    }
                    transcript.push(line);

                    // Completion requires every announced process to have
                    // terminated, so the transcript is guaranteed to carry all
                    // outcomes before we stop reading.
                    if complete_at.is_none() && tracker.complete() {
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

        let processes = tracker
            .pending
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

    /// Boots and waits for the guest to power QEMU off by itself.
    ///
    /// Success is QEMU exiting with status 0 before the deadline. Guest-initiated
    /// ACPI poweroff is the only way that happens: a panic parks in a hlt loop
    /// (deadline), a triple fault reboots (deadline). Panics on a `kernel
    /// panicked` line, on the deadline, and on a nonzero exit status.
    #[must_use]
    pub fn run_until_poweroff(self) -> Vec<String> {
        let mut child = self.boot();
        let (rx, reader) = spawn_serial_reader(&mut child);

        let mut transcript: Vec<String> = vec![];
        let deadline = Instant::now() + self.deadline;

        loop {
            let now = Instant::now();
            if now >= deadline {
                fail(
                    &transcript,
                    &mut child,
                    "deadline expired before the guest powered off".to_owned(),
                );
            }
            match rx.recv_timeout(deadline - now) {
                Ok(Err(e)) => fail(&transcript, &mut child, format!("serial read failed: {e}")),
                Ok(Ok(line)) => {
                    if line.contains("kernel panicked") {
                        transcript.push(line.clone());
                        fail(
                            &transcript,
                            &mut child,
                            format!("kernel panicked during the run: {line:?}"),
                        );
                    }
                    transcript.push(line);
                }
                Err(RecvTimeoutError::Timeout) => {
                    fail(
                        &transcript,
                        &mut child,
                        "deadline expired before the guest powered off".to_owned(),
                    );
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        let status = child.wait().expect("should be able to wait on qemu");
        let _ = reader.join();

        if !status.success() {
            dump_transcript(&transcript);
            panic!("QEMU exited with {status}, expected a guest-initiated poweroff with status 0");
        }

        transcript
    }
}

/// Takes the child's stdout and streams serial lines over a channel until
/// QEMU closes the pipe or a read fails, which is sent as the last item.
fn spawn_serial_reader(
    child: &mut Child,
) -> (mpsc::Receiver<io::Result<String>>, thread::JoinHandle<()>) {
    let stdout = child.stdout.take().expect("child stdout was not captured");
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || read_serial_lines(stdout, &tx));
    (rx, reader)
}

fn read_serial_lines(from: impl Read, tx: &mpsc::Sender<io::Result<String>>) {
    // Lines are split on raw bytes and decoded lossily, so one stray byte
    // cannot drop the rest of the transcript the way `lines()` would.
    for line in BufReader::new(from).split(b'\n') {
        let line = line.map(|mut bytes| {
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            String::from_utf8_lossy(&bytes).into_owned()
        });
        let failed = line.is_err();
        if tx.send(line).is_err() || failed {
            break;
        }
    }
}

/// Stages a runfiles image at `dest`, writable, and returns `dest`.
fn stage_writable(src: &Path, dest: &Path) -> PathBuf {
    // Runfiles are read-only build outputs, and QEMU opens the vars flash and
    // the disk read-write. Writing them in place corrupts the action cache. The
    // previous run left a read-only copy that must go before it can be replaced.
    let _ = fs::remove_file(dest);
    fs::copy(src, dest).unwrap_or_else(|e| {
        panic!(
            "should be able to stage {} at {}: {e}",
            src.display(),
            dest.display()
        )
    });

    fs::set_permissions(dest, fs::Permissions::from_mode(0o600))
        .unwrap_or_else(|e| panic!("should be able to make {} writable: {e}", dest.display()));

    dest.to_owned()
}

/// A spawned process whose outcome is still being awaited during the run.
struct PendingProcess {
    path: String,
    pid: u64,
    outcome: Option<Outcome>,
}

/// Spawn bookkeeping for one boot, fed every serial line in order.
#[derive(Default)]
struct SpawnTracker {
    pending: Vec<PendingProcess>,
    all_announced: bool,
}

impl SpawnTracker {
    /// Records `line` if it is one of the wire-contract lines, ignoring every
    /// other line. A contract line that does not parse, a spawn count that
    /// disagrees with the announcements, or an outcome for an unannounced pid
    /// is an error.
    fn observe(&mut self, line: &str) -> Result<(), String> {
        if let Some(rest) = anchor_after(line, "test-kernel: spawned ") {
            let (path, pid) =
                parse_spawned(rest).ok_or_else(|| format!("garbled spawned line {line:?}"))?;
            self.pending.push(PendingProcess {
                path,
                pid,
                outcome: None,
            });
        } else if let Some(rest) = anchor_after(line, "test-kernel: spawn complete count=") {
            let count: usize = rest
                .trim()
                .parse()
                .map_err(|_| format!("garbled spawn complete line {line:?}"))?;
            if count != self.pending.len() {
                return Err(format!(
                    "spawn complete count={count} but {} processes were announced",
                    self.pending.len()
                ));
            }
            self.all_announced = true;
        } else if let Some(rest) = anchor_after(line, "test-kernel: outcome pid=") {
            let (pid, outcome) =
                parse_outcome(rest).ok_or_else(|| format!("garbled outcome line {line:?}"))?;
            let proc = self
                .pending
                .iter_mut()
                .find(|p| p.pid == pid)
                .ok_or_else(|| format!("outcome for a pid never announced {line:?}"))?;
            proc.outcome = Some(outcome);
        }
        Ok(())
    }

    /// True once every announced process has an outcome.
    fn complete(&self) -> bool {
        self.all_announced && self.pending.iter().all(|p| p.outcome.is_some())
    }
}

/// How a spawned process ended.
#[derive(Debug, PartialEq, Eq)]
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
    /// Asserts the `index`-th spawned process exited with `expected`.
    ///
    /// # Panics
    /// Dumps the transcript and panics if the process is missing, was signaled,
    /// or exited with a different code.
    pub fn assert_exit_code(&self, index: usize, expected: u64) {
        match self.processes.get(index).map(|p| &p.outcome) {
            Some(Outcome::Exited(code)) if *code == expected => {}
            other => {
                self.dump();
                panic!("process index {index} outcome was {other:?}, expected Exited({expected})");
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

    /// Prints the whole transcript to stderr, for a test that fails on its own
    /// assertion.
    pub fn dump(&self) {
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

/// Parses the `{path} pid={N}` payload of a spawned line into `(path, pid)`.
fn parse_spawned(payload: &str) -> Option<(String, u64)> {
    let (path, pid) = payload.rsplit_once(" pid=")?;
    Some((path.trim().to_owned(), pid.trim().parse().ok()?))
}

/// Parses the `{N} exit={code}` or `{N} signal={NAME}` payload of an outcome
/// line into the pid and its outcome.
fn parse_outcome(payload: &str) -> Option<(u64, Outcome)> {
    let (pid, rest) = payload.trim().split_once(' ')?;
    let outcome = match rest.split_once('=')? {
        ("exit", code) => Outcome::Exited(code.parse().ok()?),
        ("signal", name) => Outcome::Signaled(name.to_owned()),
        _ => return None,
    };
    Some((pid.parse().ok()?, outcome))
}

/// Returns the text following `anchor`, or `None` if `anchor` is absent. Used to
/// skip any log prefix (timestamp, level, target) before a known payload.
fn anchor_after<'a>(line: &'a str, anchor: &str) -> Option<&'a str> {
    line.find(anchor).map(|idx| &line[idx + anchor.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_utf8_byte_does_not_truncate_the_transcript() {
        let (tx, rx) = mpsc::channel();
        read_serial_lines(io::Cursor::new(b"ok\n\xffbad\r\nafter\n".to_vec()), &tx);
        drop(tx);
        let lines: Vec<String> = rx.iter().map(Result::unwrap).collect();
        assert_eq!(lines, ["ok", "\u{fffd}bad", "after"]);
    }

    #[test]
    fn parse_outcome_reads_exit_and_signal() {
        assert_eq!(parse_outcome("3 exit=42"), Some((3, Outcome::Exited(42))));
        assert_eq!(
            parse_outcome("1 signal=SIGSEGV"),
            Some((1, Outcome::Signaled("SIGSEGV".to_owned())))
        );
        assert_eq!(parse_outcome("1 exit=x"), None);
    }

    fn observe_all(lines: &[&str]) -> Result<SpawnTracker, String> {
        let mut tracker = SpawnTracker::default();
        for line in lines {
            tracker.observe(line)?;
        }
        Ok(tracker)
    }

    #[test]
    fn garbled_outcome_line_is_an_error() {
        let result = observe_all(&[
            "test-kernel: spawned /bin/init pid=1",
            "test-kernel: spawn complete count=1",
            "test-kernel: outcome pid=1 exit=4\u{fffd}",
        ]);
        assert!(result.is_err(), "garbled outcome was accepted");
    }

    #[test]
    fn spawn_complete_count_must_match_the_spawned_lines() {
        let result = observe_all(&[
            "test-kernel: spawned /bin/init pid=1",
            "test-kernel: spawn complete count=2",
        ]);
        assert!(result.is_err(), "count mismatch was accepted");
    }

    #[test]
    fn outcome_for_an_unknown_pid_is_an_error() {
        let result = observe_all(&[
            "test-kernel: spawned /bin/init pid=1",
            "test-kernel: spawn complete count=1",
            "test-kernel: outcome pid=7 exit=0",
        ]);
        assert!(result.is_err(), "outcome for an unspawned pid was accepted");
    }

    #[test]
    fn completes_once_every_spawned_process_has_an_outcome() {
        let mut tracker = observe_all(&[
            "test-kernel: spawned /bin/a pid=1",
            "[INFO] test-kernel: spawned /bin/b pid=2",
            "test-kernel: spawn complete count=2",
            "test-kernel: outcome pid=2 signal=SIGSEGV",
        ])
        .unwrap();
        assert!(!tracker.complete());
        tracker
            .observe("test-kernel: outcome pid=1 exit=0")
            .unwrap();
        assert!(tracker.complete());
        assert_eq!(tracker.pending[0].outcome, Some(Outcome::Exited(0)));
    }

    fn signaled_report() -> RunReport {
        RunReport {
            transcript: vec![],
            processes: vec![SpawnedProcess {
                path: "/bin/init".to_owned(),
                pid: 1,
                outcome: Outcome::Signaled("SIGSEGV".to_owned()),
            }],
        }
    }

    #[test]
    #[should_panic(expected = "Signaled(\"SIGSEGV\")")]
    fn assert_exit_code_names_the_signal() {
        signaled_report().assert_exit_code(0, 0);
    }

    #[test]
    #[should_panic(expected = "None")]
    fn assert_exit_code_names_a_missing_process() {
        signaled_report().assert_exit_code(1, 0);
    }
}
