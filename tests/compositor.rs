//! End-to-end test for the compositor socket protocol on a virgl GPU.
//!
//! `MESA_LOADER_DRIVER_OVERRIDE=llvmpipe`: with the host radeonsi driver
//! (Mesa 26.2), QEMU 11.1 segfaults in its EGL teardown after the guest
//! powers off, and the harness counts the nonzero exit as a failed run.

use test_support::{KernelTest, host_env};

#[test]
fn compositor() {
    let report = KernelTest::new("compositor", host_env!())
        .qemu_env("MESA_LOADER_DRIVER_OVERRIDE", "llvmpipe")
        .qemu_args([
            "-device".to_owned(),
            "virtio-gpu-gl,xres=800,yres=600".to_owned(),
            "-vga".to_owned(),
            "none".to_owned(),
            "-display".to_owned(),
            "egl-headless".to_owned(),
        ])
        .run();
    report.assert_no_line_contains("FAIL");
    // The demo can connect and print before the compositor's own print lands.
    report.assert_line_contains("compositor: listening on /var/run/compositor.sock");
    report.assert_markers_in_order(&[
        "compositor: gpu scanout 800x600",
        "compositor-demo: connected",
        "compositor: frame drawn 800x600",
        "compositor: probe (400,100) dominant=red",
        "compositor: probe (400,560)=0x00000000",
        "compositor: new window /dev/shm/compositor-demo 256x128",
        "compositor: blit 256x128 first=0xff2060c0",
        "compositor-demo: window created",
        "frames in the last",
        "compositor-smoke: demo killed",
    ]);
    report.assert_exit_code(0, 0);
}
