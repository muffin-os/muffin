//! This module holds the pointer state. The state is a hotspot on the
//! screen and the pressed buttons. The module does not know how the cursor
//! looks.

use core::fmt;

use libinput::{ButtonState, Event, MouseButton};

/// This struct holds the pressed pointer buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Buttons {
    pub left: bool,
    pub right: bool,
    pub middle: bool,
}

/// Writes the buttons as `left=0 right=1 middle=0`.
impl fmt::Display for Buttons {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "left={} right={} middle={}",
            u8::from(self.left),
            u8::from(self.right),
            u8::from(self.middle)
        )
    }
}

impl Buttons {
    fn get(self, button: MouseButton) -> bool {
        match button {
            MouseButton::Left => self.left,
            MouseButton::Right => self.right,
            MouseButton::Middle => self.middle,
        }
    }

    fn set(&mut self, button: MouseButton, is_pressed: bool) {
        match button {
            MouseButton::Left => self.left = is_pressed,
            MouseButton::Right => self.right = is_pressed,
            MouseButton::Middle => self.middle = is_pressed,
        }
    }
}

/// This is the change that one input event makes to the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerChange {
    /// The hotspot is at a new place.
    Moved,
    /// A button goes down or up. A repeat of the same state is not a change.
    Button {
        button: MouseButton,
        is_pressed: bool,
    },
}

impl PointerChange {
    pub fn is_button(self) -> bool {
        matches!(self, Self::Button { .. })
    }
}

pub struct Pointer {
    x: u32,
    y: u32,
    screen_width: u32,
    screen_height: u32,
    buttons: Buttons,
}

impl Pointer {
    /// Starts at the centre of the screen with no button pressed.
    pub fn new(screen_width: u32, screen_height: u32) -> Self {
        Self {
            x: screen_width / 2,
            y: screen_height / 2,
            screen_width,
            screen_height,
            buttons: Buttons::default(),
        }
    }

    pub fn position(&self) -> (u32, u32) {
        (self.x, self.y)
    }

    pub fn buttons(&self) -> Buttons {
        self.buttons
    }

    /// Moves the hotspot for a motion event, or records a button. Other
    /// events change nothing, for example a key, a wheel turn or an absolute
    /// position.
    pub fn apply(&mut self, event: Event) -> Option<PointerChange> {
        match event {
            Event::Motion { dx, dy } => {
                let before = (self.x, self.y);
                self.x = shift(self.x, dx, self.screen_width);
                self.y = shift(self.y, dy, self.screen_height);
                (before != (self.x, self.y)).then_some(PointerChange::Moved)
            }
            Event::Button { button, state } => {
                let is_pressed = state == ButtonState::Pressed;
                if self.buttons.get(button) == is_pressed {
                    return None;
                }
                self.buttons.set(button, is_pressed);
                Some(PointerChange::Button { button, is_pressed })
            }
            Event::Key { .. } | Event::Wheel { .. } | Event::Position { .. } => None,
        }
    }
}

/// Moves `position` by `delta` and keeps it on a screen of `extent` pixels.
fn shift(position: u32, delta: i32, extent: u32) -> u32 {
    let last = extent.saturating_sub(1) as i64;
    (i64::from(position) + i64::from(delta)).clamp(0, last) as u32
}

#[cfg(test)]
mod tests {
    use libinput::{KeyCode, KeyState};

    use super::*;

    fn motion(dx: i32, dy: i32) -> Event {
        Event::Motion { dx, dy }
    }

    fn button(button: MouseButton, state: ButtonState) -> Event {
        Event::Button { button, state }
    }

    #[test]
    fn motion_clamps_to_the_screen() {
        let mut pointer = Pointer::new(800, 600);
        assert_eq!(
            pointer.apply(motion(-10_000, 10_000)),
            Some(PointerChange::Moved)
        );
        assert_eq!(pointer.position(), (0, 599));
        assert_eq!(pointer.apply(motion(-1, 1)), None, "already at the edge");
        assert_eq!(pointer.apply(motion(799, 0)), Some(PointerChange::Moved));
        assert_eq!(pointer.position(), (799, 599));
    }

    #[test]
    fn buttons_report_each_transition_once() {
        let mut pointer = Pointer::new(800, 600);
        assert_eq!(
            pointer.apply(button(MouseButton::Right, ButtonState::Pressed)),
            Some(PointerChange::Button {
                button: MouseButton::Right,
                is_pressed: true
            })
        );
        assert_eq!(
            pointer.apply(button(MouseButton::Right, ButtonState::Pressed)),
            None,
            "a second press changes nothing"
        );
        assert_eq!(
            pointer.apply(button(MouseButton::Right, ButtonState::Released)),
            Some(PointerChange::Button {
                button: MouseButton::Right,
                is_pressed: false
            })
        );
        assert_eq!(
            pointer.apply(button(MouseButton::Right, ButtonState::Released)),
            None
        );
        assert_eq!(pointer.buttons(), Buttons::default());
    }

    #[test]
    fn keys_wheel_and_positions_change_nothing() {
        let mut pointer = Pointer::new(800, 600);
        assert_eq!(pointer.apply(Event::Wheel { delta: 1 }), None);
        assert_eq!(
            pointer.apply(Event::Key {
                code: KeyCode::ESC,
                state: KeyState::Pressed
            }),
            None
        );
        assert_eq!(pointer.apply(Event::Position { x: 0, y: 0 }), None);
        assert_eq!(pointer.position(), (400, 300));
        assert_eq!(pointer.buttons(), Buttons::default());
    }
}
