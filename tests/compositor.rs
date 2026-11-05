//! End-to-end test for the compositor socket protocol on a virgl GPU.

use test_support::{KernelTest, host_env};

#[test]
fn compositor() {
    let report = KernelTest::new("compositor", host_env!())
        .virgl_gpu(800, 600)
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
        "compositor: new window 256x128",
        "compositor-demo: window created",
        "compositor: blit 256x128 first=0xff2060c0",
        "frames in the last",
        "compositor-smoke: demo killed",
    ]);
    report.assert_exit_code(0, 0);
}
