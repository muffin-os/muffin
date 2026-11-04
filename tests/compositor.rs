//! End-to-end test for the compositor socket protocol with a framebuffer.

use test_support::{KernelTest, host_env};

#[test]
fn compositor() {
    let report = KernelTest::new("compositor", host_env!())
        .qemu_args([
            "-device".to_owned(),
            "virtio-gpu,xres=800,yres=600".to_owned(),
            "-vga".to_owned(),
            "none".to_owned(),
        ])
        .run();
    report.assert_no_line_contains("FAIL");
    // The demo can connect and print before the compositor's own print lands.
    report.assert_line_contains("compositor: listening on /var/run/compositor.sock");
    report.assert_markers_in_order(&[
        "compositor-demo: connected",
        "compositor: new window /dev/shm/compositor-demo 256x128",
        "compositor: frame drawn 800x600",
        "compositor: blit 256x128 first=0xff2060c0",
        "compositor-demo: window created",
        "compositor-demo: 30 frames finished",
        "compositor-smoke: demo killed",
    ]);
    report.assert_exit_code(0, 0);
}
