//! End-to-end test for shared, file-backed mmap of the framebuffer device.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/fb-mmap` in the
//! `/spawn` manifest and a virtio-gpu device pinned to a fixed resolution. The
//! binary opens `/dev/fb0`, queries its geometry, `mmap`s the framebuffer
//! `MAP_SHARED`, stamps a pattern through the mapping, and reads the same bytes
//! back through the read syscall to prove the mapping aliases the real device
//! memory rather than a private copy. It also confirms that shared mmap of a
//! regular file and private mmap of a device file are both rejected.
//!
//! Runs are skipped on hosts without QEMU so local checkouts stay green.
//! CI installs `qemu-system-x86`.

use test_support::{KernelTest, host_env};

#[test]
fn fb_mmap() {
    let report = KernelTest::new("fb_mmap", host_env!())
        .program(
            "bin/fb-mmap",
            env!("CARGO_BIN_FILE_FB_MMAP_TEST_fb-mmap-test"),
        )
        .spawn("/bin/fb-mmap")
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
