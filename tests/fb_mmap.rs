//! End-to-end test for shared, file-backed mmap of the framebuffer device.
//!
//! `/bin/fb-mmap` prints `fb-mmap: ok` and exits 0 only if bytes written
//! through a `MAP_SHARED` mapping of `/dev/fb0` read back through the read
//! syscall, shared mmap of a regular file is rejected, and a `MAP_PRIVATE`
//! mapping never writes back. The virtio-gpu device is pinned to a fixed
//! resolution so the geometry is known.

use test_support::{KernelTest, host_env};

#[test]
fn fb_mmap() {
    let report = KernelTest::new("fb_mmap", host_env!())
        .qemu_args([
            "-device".to_owned(),
            "virtio-gpu,xres=800,yres=600".to_owned(),
            "-vga".to_owned(),
            "none".to_owned(),
        ])
        .run();

    report.assert_line_contains("fb-mmap: ok");
    report.assert_exit_code(0, 0);
}
