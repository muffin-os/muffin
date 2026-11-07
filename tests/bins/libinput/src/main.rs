//! Prints `libinput: ok`; the first failure prints `libinput: FAIL <detail>`
//! and exits 1.
//!
//! The guest reads all input devices through `libinput` on a filler thread
//! and prints each device and each event that the main thread pops, with the
//! id of its device. For each phase, it
//! prints a `libinput: await <phase>` marker. The host test
//! `tests/libinput.rs` injects the events of that phase through QMP only
//! after it reads that marker:
//!
//! - `mouse`: one batch with an X and a Y motion, then a left click.
//! - `keyboard`: Shift down, `a` down, `a` up, Shift up.

use std::fmt::Display;
use std::num::NonZeroUsize;
use std::process::exit;
use std::sync::Arc;
use std::thread;

use libinput::{
    ButtonState, DeviceEvent, Event, InputBuffer, InputDevices, KeyCode, KeyState, Keyboard,
    MouseButton,
};

fn fail(detail: impl Display) -> ! {
    println!("libinput: FAIL {detail}");
    exit(1);
}

/// Pops and prints events until `is_last` returns true for one.
fn await_phase(buffer: &InputBuffer, phase: &str, mut is_last: impl FnMut(Event) -> bool) {
    println!("libinput: await {phase}");
    loop {
        let DeviceEvent { device, event } = buffer.pop();
        println!("libinput: event {device} {event:?}");
        if is_last(event) {
            return;
        }
    }
}

fn main() {
    let mut devices = InputDevices::open_all().unwrap_or_else(|e| fail(e));
    for info in devices.devices() {
        println!("libinput: device {} {} {:?}", info.id, info.path, info.name);
    }
    let buffer = Arc::new(InputBuffer::new(NonZeroUsize::new(64).unwrap()));
    thread::spawn({
        let buffer = buffer.clone();
        move || {
            let Err(e) = devices.fill(&buffer);
            fail(e);
        }
    });

    await_phase(&buffer, "mouse", |event| {
        event
            == Event::Button {
                button: MouseButton::Left,
                state: ButtonState::Released,
            }
    });

    let mut keyboard = Keyboard::new();
    let mut typed = String::new();
    await_phase(&buffer, "keyboard", |event| {
        typed.extend(keyboard.apply(event));
        event
            == Event::Key {
                code: KeyCode::LEFT_SHIFT,
                state: KeyState::Released,
            }
    });
    println!("libinput: typed {typed:?}");
    println!("libinput: ok");
}
