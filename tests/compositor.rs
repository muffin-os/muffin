//! This end-to-end test checks the compositor socket protocol and the mouse
//! cursor on a virgl GPU. The host injects mouse events through QMP while
//! the guest runs.

use std::thread;

use test_support::{KernelTest, host_env, qmp};

/// A demo frame uses a `Damage` request and a `Finish` request.
const REQUESTS_PER_FRAME: u32 = 2;
/// This is the frame rate of a compositor that serves one request for each
/// client only when a mouse event wakes it.
const MOUSE_WOKEN_FRAMES_PER_SECOND: u32 =
    1000 / qmp::INJECT_INTERVAL.as_millis() as u32 / REQUESTS_PER_FRAME;
/// This is the minimum number of frames in one report of one second. It is
/// two times the rate of a compositor that only a mouse event wakes. Thus
/// the test shows that clients wake the loop. The demo gets near 100 frames
/// on a KVM host.
const MIN_FRAMES_PER_REPORT: u32 = 2 * MOUSE_WOKEN_FRAMES_PER_SECOND;

#[test]
fn compositor() {
    let socket = qmp::socket_path("compositor");
    let injector = {
        let socket = socket.clone();
        let commands = [
            qmp::mouse_motion('x', 5),
            qmp::mouse_button("left", true),
            qmp::mouse_button("left", false),
            qmp::mouse_button("right", true),
            qmp::mouse_button("right", false),
            qmp::mouse_button("middle", true),
            qmp::mouse_button("middle", false),
        ];
        thread::spawn(move || qmp::inject_until_exit(&socket, &commands))
    };
    let report = KernelTest::new("compositor", host_env!())
        .virgl_gpu(800, 600)
        .qemu_args(["-device".to_owned(), "virtio-mouse-pci".to_owned()])
        .qemu_args(qmp::qemu_args(&socket))
        .run();
    injector.join().expect("qmp injector thread");
    report.assert_no_line_contains("FAIL");
    // The demo can connect and print before the compositor's own print lands.
    report.assert_line_contains("compositor: listening on /var/run/compositor.sock");
    report.assert_markers_in_order(&[
        "compositor: gpu scanout 800x600",
        "compositor: input /dev/input/event0",
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
    // Each button paints the cursor, and the scanout shows the paint.
    report.assert_line_contains("left=1 right=0 middle=0 fill=primary outline=idle pixels=ok");
    report.assert_line_contains("left=0 right=1 middle=0 fill=secondary outline=idle pixels=ok");
    report.assert_line_contains("left=0 right=0 middle=1 fill=idle outline=middle pixels=ok");
    report.assert_line_contains("left=0 right=0 middle=0 fill=idle outline=idle pixels=ok");
    let frames: u32 = report
        .transcript
        .iter()
        .filter_map(|line| line.strip_prefix("compositor-demo: "))
        .filter_map(|line| line.split_once(" frames in the last"))
        .map(|(frames, _)| frames.parse().expect("frame count"))
        .max()
        .expect("a frame-rate report");
    assert!(
        frames >= MIN_FRAMES_PER_REPORT,
        "only {frames} frames in one report"
    );
    report.assert_exit_code(0, 0);
}
