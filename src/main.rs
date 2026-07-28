use clap::Parser;

static KERNEL_BINARY: &str = env!("KERNEL_BINARY");
static BOOTABLE_ISO: &str = env!("BOOTABLE_ISO");
static OVMF_CODE: &str = env!("OVMF_X86_64_CODE");
static OVMF_VARS: &str = env!("OVMF_X86_64_VARS");
static DISK_IMAGE: &str = env!("DISK_IMAGE");

#[derive(Parser)]
struct Args {
    #[arg(
        long,
        help = "Start QEMU with a GDB server listening on localhost:1234"
    )]
    debug: bool,
    #[arg(long, help = "Run QEMU without a display")]
    headless: bool,
    #[arg(long, help = "Number of CPU cores to emulate", default_value_t = 4)]
    smp: u8,
    #[arg(long, help = "Don't boot, just build")]
    no_run: bool,
    #[arg(
        long,
        help = "The amount of RAM that the emulator will boot with ('4G', '17M' etc.)",
        default_value = "4G"
    )]
    mem: String,
}

fn main() {
    println!("KERNEL_BINARY: {KERNEL_BINARY}");
    println!("BOOTABLE_ISO: {BOOTABLE_ISO}");
    println!("DISK_IMAGE: {DISK_IMAGE}");

    let args = Args::parse();

    if args.no_run {
        return;
    }

    #[cfg(debug_assertions)]
    {
        // create an lldb debug file to make debugging easy
        let content = format!(
            r"target create {KERNEL_BINARY}
gdb-remote localhost:1234
b kernel_main
b handle_panic
continue"
        );
        std::fs::write("debug.lldb", content).expect("unable to create lldb debug file");

        // create a gdb debug file to make debugging easy
        let content = format!(
            r"file {KERNEL_BINARY}
        target remote localhost:1234
        hbreak kernel_main
        hbreak kernel::handle_panic
        continue"
        );
        std::fs::write("debug.gdb", content).expect("unable to create gdb debug file");

        println!(
            "debug file is ready, run `lldb -s debug.lldb` or `gdb -x debug.gdb` to start debugging"
        );
    }

    let mut cmd = std::process::Command::new("qemu-system-x86_64");
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));

    // serial comms via console - needed for log output of the kernel
    cmd.arg("-serial");
    cmd.arg("stdio");

    // QEMU monitor via telnet
    cmd.arg("-monitor");
    cmd.arg("telnet::45454,server,nowait");

    // start GDB server
    cmd.arg("-s");

    if args.debug {
        // wait for client to connect
        cmd.arg("-S");
    }

    if args.headless {
        // run without a window, but with graphics devices attached
        cmd.arg("-nographic");
    }

    cmd.arg("-m");
    cmd.arg(args.mem);

    // OVMF firmware
    cmd.arg("-drive");
    cmd.arg(format!(
        "if=pflash,unit=0,format=raw,file={OVMF_CODE},readonly=on"
    ));
    cmd.arg("-drive");
    cmd.arg(format!("if=pflash,unit=1,format=raw,file={OVMF_VARS}"));

    // kernel binary
    cmd.arg("-cdrom");
    cmd.arg(BOOTABLE_ISO);

    cmd.arg("-cpu");
    cmd.arg("max");

    cmd.arg("-smp");
    cmd.arg(args.smp.to_string());

    cmd.arg("-drive");
    cmd.arg(format!(
        "id=virtio-disk0,file={DISK_IMAGE},format=raw,if=none"
    ));
    cmd.arg("-device");
    cmd.arg("virtio-blk-pci,drive=virtio-disk0");

    // Prefer KVM, falling back to TCG so the runner still boots on hosts
    // without /dev/kvm instead of failing to launch QEMU.
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        cmd.arg("-machine");
        cmd.arg("accel=kvm:tcg");
    }

    cmd.arg("-device");
    cmd.arg("virtio-gpu,id=virtio-gpu0");
    cmd.arg("-vga");
    cmd.arg("none");

    cmd.stdout(std::process::Stdio::piped());

    let mut child = cmd.spawn().expect("unable to start qemu");

    if args.debug {
        println!("qemu is waiting for a debugger to attach on localhost:1234...");
    } else {
        println!("booting...");
    }

    let stdout = child.stdout.take().expect("qemu stdout is piped");
    forward_kernel_output(stdout, std::io::stdout().lock());

    let status = child.wait().expect("unable to wait for qemu");
    assert!(status.success());
}

/// Every kernel serial record starts with this dim timestamp prefix.
/// Firmware and bootloader output never contains it.
const KERNEL_RECORD_MARKER: &[u8] = b"\x1b[2m[";

/// Bytes buffered while waiting for the first kernel record before giving up
/// and passing everything through.
const GATE_LIMIT: usize = 64 * 1024;

/// Copies the QEMU serial stream to `to`, dropping everything before the
/// first kernel log record.
///
/// OVMF mirrors the UEFI console to the serial port, so firmware and Limine
/// messages precede the kernel's output and cannot be silenced through
/// limine.conf. If the kernel never produces a record, the suppressed bytes
/// are flushed on QEMU exit (or once [`GATE_LIMIT`] is exceeded) so boot
/// failures stay diagnosable.
fn forward_kernel_output(mut from: impl std::io::Read, mut to: impl std::io::Write) {
    let mut chunk = [0_u8; 4096];
    let mut pending: Vec<u8> = vec![];
    let mut forwarding = false;

    loop {
        let n = match from.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if forwarding {
            to.write_all(&chunk[..n])
                .expect("unable to write serial output");
            let _ = to.flush();
            continue;
        }

        pending.extend_from_slice(&chunk[..n]);
        let start = pending
            .windows(KERNEL_RECORD_MARKER.len())
            .position(|window| window == KERNEL_RECORD_MARKER);
        if let Some(start) = start {
            forwarding = true;
            to.write_all(&pending[start..])
                .expect("unable to write serial output");
            let _ = to.flush();
            pending = vec![];
        } else if pending.len() > GATE_LIMIT {
            forwarding = true;
            to.write_all(&pending)
                .expect("unable to write serial output");
            let _ = to.flush();
            pending = vec![];
        }
    }

    if !forwarding {
        // The kernel never came up. Show what the firmware had to say.
        let _ = to.write_all(&pending);
        let _ = to.flush();
    }
}
