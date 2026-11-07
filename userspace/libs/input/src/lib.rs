//! Keyboard and mouse input for userspace programs.
//!
//! - [`Event`] is one input event. It does not depend on the source of the
//!   events. [`DeviceEvent`] adds the [`DeviceId`] of the device that made
//!   the event, so a consumer can tell the devices apart.
//! - [`InputBuffer`] is a ring buffer of device events between a filler
//!   thread and a consumer thread.
//! - `InputDevices` (only on muffin) reads all `/dev/input/eventN` nodes and
//!   fills an [`InputBuffer`]. `DeviceInfo` gives the id, the path and the
//!   name of each node. [`Decoder`] turns the raw events of one node into
//!   [`Event`]s.
//! - [`Keyboard`] turns key presses into characters of the US layout.
//!
//! The library does not start a thread. Start a thread and call
//! `InputDevices::fill` on it:
//!
//! ```ignore
//! use std::num::NonZeroUsize;
//! use std::sync::Arc;
//! use std::thread;
//!
//! use libinput::{InputBuffer, InputDevices, Keyboard};
//!
//! let buffer = Arc::new(InputBuffer::new(NonZeroUsize::new(256).unwrap()));
//! let mut devices = InputDevices::open_all()?;
//! thread::spawn({
//!     let buffer = buffer.clone();
//!     move || {
//!         let Err(e) = devices.fill(&buffer);
//!         eprintln!("input: {e}");
//!     }
//! });
//!
//! let mut keyboard = Keyboard::new();
//! loop {
//!     if let Some(c) = keyboard.apply(buffer.pop().event) {
//!         print!("{c}");
//!     }
//! }
//! ```

mod buffer;
mod decode;
#[cfg(target_os = "muffin")]
mod device;
mod event;
mod keymap;

pub use buffer::InputBuffer;
pub use decode::Decoder;
#[cfg(target_os = "muffin")]
pub use device::{DeviceInfo, InputDevices};
pub use event::{ButtonState, DeviceEvent, DeviceId, Event, KeyCode, KeyState, MouseButton};
pub use keymap::{BACKSPACE, ENTER, Keyboard, TAB};
