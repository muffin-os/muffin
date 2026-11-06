//! Prints `input-nodes: ok`; the first failure prints
//! `input-nodes: FAIL <step> <detail>` and exits 1.
//!
//! The guest finds the mouse and the keyboard by their supported codes. For
//! each phase, it discards old events, prints an `input-nodes: await <phase>`
//! marker, and waits. The host test `tests/input_nodes.rs` injects the events
//! of a phase through QMP only after it reads that marker:
//!
//! - `mouse`: one motion on the X axis with value 5.
//! - `keyboard`: one press of the `a` key.
//! - `batch`: ten motions on the X axis with value 5.
//!
//! The first two phases block in one `poll` without a timeout over both
//! descriptors. Only the descriptor of the injected device must be readable.
//!
//! A second open file of the mouse must get each event that the first file
//! reads. Thus the guest reads the batch through the first file. Then it checks
//! that the second file holds the same batch, without a wait.

use std::ffi::c_int;
use std::process::exit;
use std::time::Duration;

use kernel_abi::{InputEvent, InputEventKind, MotionAxis, PollEvents, PollFd};
use muffin_sys::{Errno, O_RDONLY};

const KEY_A: u16 = 30;
/// This is the number of motion events that the first reader collects
/// before the test checks the second reader.
const BATCH_MOTIONS: usize = 10;
/// The host injects each X motion with this value.
const MOTION_X: i32 = 5;

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

/// Reads all the events that `fd` holds now. Returns no events when a read blocks.
fn drain(fd: c_int) -> Vec<InputEvent> {
    let mut fds = [PollFd::new(fd, PollEvents::IN)];
    let ready = unwrap_or_fail("poll", muffin_sys::poll(&mut fds, Some(Duration::ZERO)));
    if ready == 0 {
        return Vec::new();
    }
    let mut buf = [0u8; InputEvent::SIZE * 4096];
    let n = unwrap_or_fail("read", muffin_sys::read(fd, &mut buf));
    buf[..n]
        .as_chunks::<{ InputEvent::SIZE }>()
        .0
        .iter()
        .map(|raw| InputEvent::from_bytes(*raw))
        .collect()
}

/// Blocks in one `poll` without a timeout over both descriptors. Then
/// requires that only `ready` is readable.
fn await_only(ready: c_int, idle: c_int) {
    let mut fds = [
        PollFd::new(ready, PollEvents::IN),
        PollFd::new(idle, PollEvents::IN),
    ];
    let count = unwrap_or_fail("poll", muffin_sys::poll(&mut fds, None));
    if count != 1 || fds[0].revents != PollEvents::IN || !fds[1].revents.is_empty() {
        fail("poll-revents", (count, fds));
    }
}

fn count_motions(events: &[InputEvent]) -> usize {
    events
        .iter()
        .filter(|event| event.matches(InputEventKind::Motion, MotionAxis::X))
        .count()
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
            (true, false) => mouse = Some((path, fd)),
            (false, true) => keyboard = Some(fd),
            other => fail(path, other),
        }
    }
    let (mouse_path, mouse) = mouse.unwrap_or_else(|| fail("mouse", "no device with motion on x"));
    let keyboard = keyboard.unwrap_or_else(|| fail("keyboard", "no device with KEY_A"));

    drain(mouse);
    drain(keyboard);

    println!("input-nodes: await mouse");
    await_only(mouse, keyboard);
    let motion = wait_for(mouse, InputEventKind::Motion, MotionAxis::X);
    if motion.value != MOTION_X {
        fail("motion-value", motion);
    }
    println!("input-nodes: motion x value={}", motion.value);
    drain(mouse);

    println!("input-nodes: await keyboard");
    await_only(keyboard, mouse);
    let press = wait_for(keyboard, InputEventKind::Key, KEY_A);
    if press.value != 1 {
        fail("key-value", press);
    }
    println!("input-nodes: key a value={}", press.value);
    drain(keyboard);

    // The second file does not get the events that the first file queued
    // before the second open. Thus the test discards them first.
    let second = unwrap_or_fail("second-open", muffin_sys::open(mouse_path, O_RDONLY));
    drain(mouse);
    println!("input-nodes: await batch");
    for _ in 0..BATCH_MOTIONS {
        let motion = wait_for(mouse, InputEventKind::Motion, MotionAxis::X);
        if motion.value != MOTION_X {
            fail("batch-value", motion);
        }
    }
    let second_motions = count_motions(&drain(second));
    println!("input-nodes: second reader motions={second_motions}");
    if second_motions != BATCH_MOTIONS {
        fail("second-reader", second_motions);
    }

    println!("input-nodes: ok");
}
