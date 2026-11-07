//! The input events that a consumer reads. The types do not refer to a
//! device file, so a source other than `/dev/input` can make the same events.

use std::fmt;

/// A keyboard key, identified by its Linux evdev code. Every `u16` is a
/// valid code, also a code that has no constant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyCode(pub u16);

impl KeyCode {
    pub const ESC: Self = Self(1);
    pub const BACKSPACE: Self = Self(14);
    pub const TAB: Self = Self(15);
    pub const ENTER: Self = Self(28);
    pub const LEFT_CTRL: Self = Self(29);
    pub const LEFT_SHIFT: Self = Self(42);
    pub const RIGHT_SHIFT: Self = Self(54);
    pub const LEFT_ALT: Self = Self(56);
    pub const SPACE: Self = Self(57);
    pub const CAPS_LOCK: Self = Self(58);
    pub const RIGHT_CTRL: Self = Self(97);
    pub const RIGHT_ALT: Self = Self(100);
    pub const UP: Self = Self(103);
    pub const LEFT: Self = Self(105);
    pub const RIGHT: Self = Self(106);
    pub const DOWN: Self = Self(108);
    pub const DELETE: Self = Self(111);
}

/// The change that a key event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Pressed,
    Released,
    /// The key stays down, and the keyboard repeats the press.
    Repeated,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// The change that a mouse button event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Pressed,
    Released,
}

/// One input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A keyboard key changes. A mouse button is a [`Event::Button`], not a key.
    Key { code: KeyCode, state: KeyState },
    /// A mouse button changes.
    Button {
        button: MouseButton,
        state: ButtonState,
    },
    /// The mouse moves by this distance. Positive `dx` is to the right and
    /// positive `dy` is down.
    Motion { dx: i32, dy: i32 },
    /// The mouse wheel turns by this number of steps. A positive value turns
    /// the wheel away from the user.
    Wheel { delta: i32 },
    /// An absolute pointer, for example a tablet, is at this position. The
    /// unit and the range are those of the device.
    Position { x: i32, y: i32 },
}

/// Identifies one input device of a source. The source gives each device
/// its own id, and an id stays with its device while the source runs. For
/// `/dev/input/eventN`, the id is `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub u32);

/// Writes the id as its number.
impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One input event and the device that made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceEvent {
    pub device: DeviceId,
    pub event: Event,
}
