//! End-to-end signal delivery test.
//!
//! Boots a dedicated test kernel (`tests/kernels/signals/kernel`) under QEMU
//! with two instances of a dedicated test init (`tests/kernels/signals/init`,
//! a driver and a victim), assembled into their own ISO/disk image.
//!
//! The two processes coordinate with a ping/ack handshake over SIGWINCH and
//! SIGURG instead of racing on wall-clock delays. The driver pings the victim
//! and counts the replies it gets back across each phase, then prints one
//! counter report that this test asserts verbatim. A reply can only arrive
//! when the victim is actually scheduled, so the counts prove stop, continue,
//! and terminate really took effect.
//!
//! This exercises masking, sigpending, handler entry, sigreturn restore,
//! remote stop/continue, remote default-terminate, and catchable SIGSEGV. The
//! production kernel and init stay free of test-only branches.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use test_support::{DiskFile, build_disk_image, build_iso};

const OVMF_CODE: &str = env!("OVMF_X86_64_CODE");
const OVMF_VARS: &str = env!("OVMF_X86_64_VARS");
const LIMINE_DIR: &str = env!("LIMINE_DIR");
const TEST_KERNEL_BINARY: &str = env!("CARGO_BIN_FILE_SIGNALS_TEST_KERNEL_signals-test-kernel");
const TEST_INIT_BINARY: &str = env!("CARGO_BIN_FILE_SIGNALS_TEST_INIT_signals-test-init");

/// Overall wall-clock budget. QEMU under TCG in CI is slow.
const OVERALL_DEADLINE: Duration = Duration::from_secs(180);
/// Extra window to keep draining serial output after the last marker appears.
const FINAL_MARKER_GRACE: Duration = Duration::from_secs(2);

/// Ordered markers, each expected at or after the previous one.
const MARKERS: [&str; 9] = [
    "A: start",
    "A: pending ok",
    "A: handler ran",
    "A: after unblock",
    "stopping process",
    "continuing process",
    "terminating process on signal SIGTERM",
    "A: report ready=1 pre=1 stop=0 cont=1 term=0",
    "A: segv handled",
];

/// Index of the final marker inside `MARKERS`.
const FINAL_MARKER: usize = 8;

/// Assembles a dedicated ISO and ext2 disk that boot the signals test kernel
/// with the signals test init at `/bin/init`, instead of the production
/// kernel/init.
fn build_test_image() -> (PathBuf, PathBuf) {
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-images/signals");
    fs::create_dir_all(&out_dir).expect("should be able to create test image output directory");

    let init_bytes =
        fs::read(TEST_INIT_BINARY).expect("should be able to read the signals test init binary");
    let disk = build_disk_image(
        &[DiskFile {
            path: "bin/init",
            content: init_bytes,
        }],
        &out_dir,
    );

    let limine_conf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("limine.conf");
    let iso = build_iso(
        Path::new(LIMINE_DIR),
        &limine_conf,
        Path::new(TEST_KERNEL_BINARY),
        &out_dir,
    );

    (iso, disk)
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

/// Print the full transcript, kill the child, then fail the test.
fn fail(transcript: &[String], child: &mut Child, message: String) -> ! {
    dump_transcript(transcript);
    let _ = child.kill();
    let _ = child.wait();
    panic!("{message}");
}

#[test]
fn signal_delivery_end_to_end() {
    // Skip on hosts without QEMU (local runs); CI installs it.
    if Command::new("qemu-system-x86_64")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        println!("skipping: qemu-system-x86_64 not found");
        return;
    }

    let (bootable_iso, disk_image) = build_test_image();
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
            "if=pflash,unit=0,format=raw,file={OVMF_CODE},readonly=on"
        ))
        .arg("-drive")
        .arg(format!("if=pflash,unit=1,format=raw,file={OVMF_VARS}"))
        .arg("-cdrom")
        .arg(&bootable_iso)
        .arg("-cpu")
        .arg("max")
        .arg("-smp")
        .arg("4")
        .arg("-drive")
        .arg(format!(
            "id=virtio-disk0,file={},format=raw,if=none",
            disk_image.display()
        ))
        .arg("-device")
        .arg("virtio-blk-pci,drive=virtio-disk0")
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
    let deadline = Instant::now() + OVERALL_DEADLINE;
    let mut final_seen_at: Option<Instant> = None;
    let mut child_exited_early = false;

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        // Once the final marker appears, only wait out the grace window.
        let timeout = match final_seen_at {
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
                if final_seen_at.is_none() && line.contains(MARKERS[FINAL_MARKER]) {
                    final_seen_at = Some(Instant::now());
                }
                transcript.push(line);
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => {
                // stdout closed: QEMU exited on its own.
                child_exited_early = final_seen_at.is_none();
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
            "QEMU exited before the final marker; kernel likely panicked or triple-faulted"
                .to_owned(),
        );
    }

    // The report line is the load-bearing assertion. A missing report and a
    // wrong-count report both break the ordered scan the same way, so diagnose
    // them here where the actual line is visible.
    if !transcript.iter().any(|line| line.contains("A: report ")) {
        fail(
            &transcript,
            &mut child,
            "the driver never printed its 'A: report ' line".to_owned(),
        );
    }
    if !transcript.iter().any(|line| line.contains(MARKERS[7]))
        && let Some(actual) = transcript.iter().find(|line| line.contains("A: report "))
    {
        fail(
            &transcript,
            &mut child,
            format!(
                "report line did not match {:?}, found {actual:?}",
                MARKERS[7]
            ),
        );
    }

    // Locate each marker at or after the previous marker's line.
    let mut search_from = 0usize;
    for (i, marker) in MARKERS.iter().enumerate() {
        match transcript[search_from..]
            .iter()
            .position(|line| line.contains(marker))
        {
            Some(offset) => {
                let idx = search_from + offset;
                search_from = idx;
            }
            None => fail(
                &transcript,
                &mut child,
                format!("marker {i} ({marker:?}) not found at or after the previous marker"),
            ),
        }
    }

    if let Some(line) = transcript
        .iter()
        .find(|line| line.contains("A: UNREACHABLE"))
    {
        fail(
            &transcript,
            &mut child,
            format!("found forbidden 'A: UNREACHABLE' marker: {line:?}"),
        );
    }

    if let Some(line) = transcript.iter().find(|line| line.contains("panicked")) {
        fail(
            &transcript,
            &mut child,
            format!("kernel panicked during the run: {line:?}"),
        );
    }
}
