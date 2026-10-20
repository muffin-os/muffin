//! End-to-end test for the ioctl syscall and the framebuffer screen-info query.
//!
//! `/bin/fb-ioctl` prints the geometry `FbGetScreenInfo` reports for
//! `/dev/fb0`, which must match the resolution the virtio-gpu device was
//! pinned to, and `enotty ok` only if the same request on a regular file fails
//! with `ENOTTY`. Two resolutions prove the value is read, not assumed.

use test_support::{KernelTest, host_env};

fn query_at(name: &'static str, xres: u32, yres: u32) {
    let report = KernelTest::new(name, host_env!())
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
