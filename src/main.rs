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

    // The ISO is immutable at run time, so RUST_LOG rides the kernel cmdline
    // via an SMBIOS-supplied Limine config.
    if let Ok(value) = std::env::var("RUST_LOG")
        && !value.is_empty()
    {
        let conf_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("limine.conf");
        let conf = std::fs::read_to_string(&conf_path).expect("unable to read limine.conf");

        let mut lines: Vec<String> = conf.lines().map(str::to_owned).collect();
        if let Some(line) = lines
            .iter_mut()
            .find(|l| l.trim_start().starts_with("cmdline:"))
        {
            line.push_str(&format!(" RUST_LOG={value}"));
        } else if let Some(idx) = lines
            .iter()
            .position(|l| l.trim_start().starts_with("kernel_path:"))
        {
            lines.insert(idx + 1, format!("    cmdline: RUST_LOG={value}"));
        } else {
            panic!("limine.conf has neither a cmdline nor a kernel_path entry to carry RUST_LOG");
        }

        // Limine drops the last byte of the config if the trailing newline is missing.
        let modified = format!("limine:config:{}\n", lines.join("\n"));
        let smbios_path = std::env::temp_dir().join("muffin-limine.conf");
        std::fs::write(&smbios_path, modified).expect("unable to write SMBIOS limine config");

        cmd.arg("-smbios");
        cmd.arg(format!("type=11,path={}", smbios_path.display()));
    }

    let status = cmd.status().unwrap();
    assert!(status.success());
}
