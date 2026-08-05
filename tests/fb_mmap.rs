//! End-to-end test for shared, file-backed mmap of the framebuffer device.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/fb-mmap` in the
//! `/spawn` manifest and a virtio-gpu device pinned to a fixed resolution. The
//! binary opens `/dev/fb0`, queries its geometry, `mmap`s the framebuffer
//! `MAP_SHARED`, stamps a pattern through the mapping, and reads the same bytes
//! back through the read syscall to prove the mapping aliases the real device
//! memory rather than a private copy. It also confirms that shared mmap of a
//! regular file is rejected, and that a `MAP_PRIVATE` mapping of the same
//! device sees the framebuffer content but never writes back to it.

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
