//! This module holds the pointer state. The state is a hotspot on the
//! screen and the pressed buttons. The module does not know how the cursor
//! looks.

use core::fmt;

use kernel_abi::{Button, InputEvent, InputEventKind, MotionAxis};

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
    fn get(self, button: Button) -> bool {
        match button {
            Button::Left => self.left,
            Button::Right => self.right,
            Button::Middle => self.middle,
        }
    }

    fn set(&mut self, button: Button, is_pressed: bool) {
        match button {
            Button::Left => self.left = is_pressed,
            Button::Right => self.right = is_pressed,
            Button::Middle => self.middle = is_pressed,
        }
    }
}

/// This is the change that one input event makes to the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerChange {
    /// The hotspot is at a new place.
    Moved,
    /// A button goes down or up. A repeat of the same state is not a change.
    Button { button: Button, is_pressed: bool },
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

    /// Moves the hotspot for a motion event, or records a button. Other events
    /// change nothing, for example a sync, a wheel turn or an absolute position.
    pub fn apply(&mut self, event: InputEvent) -> Option<PointerChange> {
        match event.kind()? {
            InputEventKind::Motion => {
                let (x, y) = (self.x, self.y);
                match event.motion_axis()? {
                    MotionAxis::X => self.x = shift(self.x, event.value, self.screen_width),
                    MotionAxis::Y => self.y = shift(self.y, event.value, self.screen_height),
                    MotionAxis::Wheel => return None,
                }
                ((x, y) != (self.x, self.y)).then_some(PointerChange::Moved)
            }
            InputEventKind::Key => {
                let button = event.button()?;
                let is_pressed = event.is_pressed();
                if self.buttons.get(button) == is_pressed {
                    return None;
                }
                self.buttons.set(button, is_pressed);
                Some(PointerChange::Button { button, is_pressed })
            }
            InputEventKind::Sync | InputEventKind::Position => None,
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
    use super::*;

    fn motion(axis: MotionAxis, value: i32) -> InputEvent {
        InputEvent {
            event_type: InputEventKind::Motion.into(),
            code: axis.into(),
            value,
        }
    }

    fn key(button: Button, value: i32) -> InputEvent {
        InputEvent {
            event_type: InputEventKind::Key.into(),
            code: button.into(),
            value,
        }
    }

    #[test]
    fn motion_clamps_to_the_screen() {
        let mut pointer = Pointer::new(800, 600);
        assert_eq!(
            pointer.apply(motion(MotionAxis::X, -10_000)),
            Some(PointerChange::Moved)
        );
        assert_eq!(
            pointer.apply(motion(MotionAxis::Y, 10_000)),
            Some(PointerChange::Moved)
        );
        assert_eq!(pointer.position(), (0, 599));
        assert_eq!(
            pointer.apply(motion(MotionAxis::X, -1)),
            None,
            "already at the edge"
        );
        assert_eq!(
            pointer.apply(motion(MotionAxis::X, 799)),
            Some(PointerChange::Moved)
        );
        assert_eq!(pointer.position(), (799, 599));
    }

    #[test]
    fn buttons_report_each_transition_once() {
        let mut pointer = Pointer::new(800, 600);
        assert_eq!(
            pointer.apply(key(Button::Right, 1)),
            Some(PointerChange::Button {
                button: Button::Right,
                is_pressed: true
            })
        );
        assert_eq!(
            pointer.apply(key(Button::Right, 2)),
            None,
            "a key repeat changes nothing"
        );
        assert_eq!(
            pointer.apply(key(Button::Right, 0)),
            Some(PointerChange::Button {
                button: Button::Right,
                is_pressed: false
            })
        );
        assert_eq!(pointer.apply(key(Button::Right, 0)), None);
        assert_eq!(pointer.buttons(), Buttons::default());
    }

    #[test]
    fn wheel_sync_and_unknown_events_change_nothing() {
        let mut pointer = Pointer::new(800, 600);
        assert_eq!(pointer.apply(motion(MotionAxis::Wheel, 1)), None);
        assert_eq!(pointer.apply(InputEvent::default()), None);
        assert_eq!(
            pointer.apply(InputEvent {
                event_type: 0x7F,
                code: 0,
                value: 1
            }),
            None
        );
        assert_eq!(pointer.position(), (400, 300));
    }
}
