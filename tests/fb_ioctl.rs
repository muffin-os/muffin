//! End-to-end test for the ioctl syscall and the framebuffer screen-info query.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/fb-ioctl` in the
//! `/spawn` manifest and a virtio-gpu device pinned to a fixed resolution. The
//! binary opens `/dev/fb0`, issues an `FbGetScreenInfo` ioctl, and prints the
//! reported geometry, then confirms the `ENOTTY` default path on a regular
//! file. The test asserts on its serial output and exit code at two
//! resolutions.
//!
//! Runs are skipped on hosts without QEMU so local checkouts stay green.
//! CI installs `qemu-system-x86`.

use test_support::{KernelTest, host_env};

fn query_at(name: &'static str, xres: u32, yres: u32) {
    let report = KernelTest::new(name, host_env!())
        .program(
            "bin/fb-ioctl",
            env!("CARGO_BIN_FILE_FB_IOCTL_TEST_fb-ioctl-test"),
        )
        .spawn("/bin/fb-ioctl")
        .qemu_args([
            "-device".to_owned(),
            format!("virtio-gpu,xres={xres},yres={yres}"),
            "-vga".to_owned(),
            "none".to_owned(),
        ])
        .run();

    report.assert_line_contains(&format!(
        "fb-ioctl: info {xres}x{yres} pitch={} bpp=32",
        xres * 4
    ));
    report.assert_line_contains("fb-ioctl: enotty ok");
    report.assert_exit_code(0, 0);
}

#[test]
fn fb_ioctl_800x600() {
    query_at("fb_ioctl_800x600", 800, 600);
}

#[test]
fn fb_ioctl_1280x720() {
    query_at("fb_ioctl_1280x720", 1280, 720);
}
