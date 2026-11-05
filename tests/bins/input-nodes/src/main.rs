//! Prints `input-nodes: ok`; the first failure prints
//! `input-nodes: FAIL <step> <detail>` and exits 1.
//!
//! The host test `tests/input_nodes.rs` injects mouse motion on the X axis
//! and presses the `a` key through QMP until the guest powers off. The guest
//! finds the mouse and the keyboard by their supported codes, waits for one
//! event of each, and prints the value of each event. The host checks the
//! printed values against what it injected.

use std::ffi::c_int;
use std::process::exit;

use kernel_abi::{InputEvent, InputEventKind, MotionAxis};
use muffin_sys::{Errno, O_RDONLY};

const KEY_A: u16 = 30;

fn fail(step: &str, detail: impl std::fmt::Debug) -> ! {
    println!("input-nodes: FAIL {step} {detail:?}");
    exit(1);
}

fn unwrap_or_fail<T>(step: &str, result: Result<T, Errno>) -> T {
    result.unwrap_or_else(|e| fail(step, e))
}

fn next_event(fd: c_int) -> InputEvent {
    let mut buf = [0u8; InputEvent::SIZE];
    let n = unwrap_or_fail("read", muffin_sys::read(fd, &mut buf));
    if n != InputEvent::SIZE {
        fail("read", n);
    }
    InputEvent::from_bytes(buf)
}

/// Reads until an event of `kind` with `code` arrives. A sync event in
/// between is allowed; every other event is a failure.
fn wait_for(fd: c_int, kind: InputEventKind, code: impl Into<u16> + Copy) -> InputEvent {
    loop {
        let event = next_event(fd);
        if event.matches(kind, code) {
            return event;
        }
        if !event.is_sync() {
            fail("unexpected-event", event);
        }
    }
}

fn main() {
    let mut mouse = None;
    let mut keyboard = None;
    for path in ["/dev/input/event0", "/dev/input/event1"] {
        let fd = unwrap_or_fail(path, muffin_sys::open(path, O_RDONLY));
        let info = unwrap_or_fail(path, muffin_sys::input_device_info(fd));
        let name = info
            .name()
            .unwrap_or_else(|| fail(path, info.name_len_bytes));
        println!("input-nodes: {path} is {name:?}");

        let has_motion_x = unwrap_or_fail(
            "codes",
            muffin_sys::input_supported_codes(fd, InputEventKind::Motion),
        )
        .contains(MotionAxis::X);
        let has_key_a = unwrap_or_fail(
            "codes",
            muffin_sys::input_supported_codes(fd, InputEventKind::Key),
        )
        .contains(KEY_A);
        match (has_motion_x, has_key_a) {
            (true, false) => mouse = Some(fd),
            (false, true) => keyboard = Some(fd),
            other => fail(path, other),
        }
    }
    let mouse = mouse.unwrap_or_else(|| fail("mouse", "no device with motion on x"));
    let keyboard = keyboard.unwrap_or_else(|| fail("keyboard", "no device with KEY_A"));

    let motion = wait_for(mouse, InputEventKind::Motion, MotionAxis::X);
    println!("input-nodes: motion x value={}", motion.value);
    let press = wait_for(keyboard, InputEventKind::Key, KEY_A);
    println!("input-nodes: key a value={}", press.value);

    println!("input-nodes: ok");
}
