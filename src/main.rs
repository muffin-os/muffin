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

# If the kernel is a position independent executable (PIE), you need to set the slide as the offset
# at which the kernel is being loaded. For static executables, the slide is 0, in which case
# we can omit this whole line.
#
# target modules load --file {KERNEL_BINARY} --slide 0xffffffff80000000

gdb-remote localhost:1234
b kernel_main
b handle_panic
continue"
        );
        std::fs::write("debug.lldb", content).expect("unable to create debug file");
        println!("debug file is ready, run `lldb -s debug.lldb` to start debugging");
    }

    let mut cmd = std::process::Command::new("qemu-system-x86_64");

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
    cmd.arg("4G");

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

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        cmd.arg("-accel");
        cmd.arg("kvm");
    }

    cmd.arg("-device");
    cmd.arg("virtio-gpu,id=virtio-gpu0");
    cmd.arg("-vga");
    cmd.arg("none");

    let status = cmd.status().unwrap();
    assert!(status.success());
}
