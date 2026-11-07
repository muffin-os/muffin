use kernel_abi::{
    Button, InputEvent as RawEvent, InputEventKind, MotionAxis, PositionAxis, SyncCode,
};

use crate::{ButtonState, Event, KeyCode, KeyState, MouseButton};

/// Turns the raw event stream of one `/dev/input/eventN` file into
/// [`Event`]s. Use one decoder for each file, because the decoder keeps the
/// state of the current batch.
///
/// The decoder keeps the events of a batch until the batch ends. Then it
/// gives the sum of the motions and of the wheel steps, then the new
/// position, then the keys and buttons in their sequence. The pointer changes
/// go first, so that a click in the same batch applies at the new place.
#[derive(Debug, Default)]
pub struct Decoder {
    dx: i32,
    dy: i32,
    wheel: i32,
    x: Option<i32>,
    y: Option<i32>,
    is_position_changed: bool,
    keys: Vec<Event>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `raw` to the current batch. At the end of the batch, appends
    /// the events of the batch to `out`. Ignores an event that it does not
    /// know.
    pub fn decode(&mut self, raw: RawEvent, out: &mut Vec<Event>) {
        let Some(kind) = raw.kind() else {
            return;
        };
        match kind {
            InputEventKind::Sync => match SyncCode::try_from(raw.code) {
                Ok(SyncCode::BatchEnd) => self.end_batch(out),
                // The kernel discards the rest of the current batch.
                Ok(SyncCode::EventsLost) => self.discard_batch(),
                Err(_) => {}
            },
            InputEventKind::Key => {
                if let Some(event) = key_event(raw) {
                    self.keys.push(event);
                }
            }
            InputEventKind::Motion => match MotionAxis::try_from(raw.code) {
                Ok(MotionAxis::X) => self.dx = self.dx.saturating_add(raw.value),
                Ok(MotionAxis::Y) => self.dy = self.dy.saturating_add(raw.value),
                Ok(MotionAxis::Wheel) => self.wheel = self.wheel.saturating_add(raw.value),
                Err(_) => {}
            },
            InputEventKind::Position => {
                match PositionAxis::try_from(raw.code) {
                    Ok(PositionAxis::X) => self.x = Some(raw.value),
                    Ok(PositionAxis::Y) => self.y = Some(raw.value),
                    Err(_) => return,
                }
                self.is_position_changed = true;
            }
        }
    }

    fn end_batch(&mut self, out: &mut Vec<Event>) {
        if (self.dx, self.dy) != (0, 0) {
            out.push(Event::Motion {
                dx: self.dx,
                dy: self.dy,
            });
        }
        if self.wheel != 0 {
            out.push(Event::Wheel { delta: self.wheel });
        }
        // A device can report one axis before the other. Without both
        // coordinates there is no position yet.
        if self.is_position_changed
            && let (Some(x), Some(y)) = (self.x, self.y)
        {
            out.push(Event::Position { x, y });
        }
        out.append(&mut self.keys);
        self.discard_batch();
    }

    /// Keeps the last position, because the next batch can report only the
    /// axis that changes.
    fn discard_batch(&mut self) {
        self.dx = 0;
        self.dy = 0;
        self.wheel = 0;
        self.is_position_changed = false;
        self.keys.clear();
    }
}

/// Returns `None` for a value that is not a known key state, and for a
/// button repeat, which is not a change.
fn key_event(raw: RawEvent) -> Option<Event> {
    if let Ok(button) = Button::try_from(raw.code) {
        let button = match button {
            Button::Left => MouseButton::Left,
            Button::Right => MouseButton::Right,
            Button::Middle => MouseButton::Middle,
        };
        let state = match raw.value {
            0 => ButtonState::Released,
            1 => ButtonState::Pressed,
            _ => return None,
        };
        return Some(Event::Button { button, state });
    }
    let state = match raw.value {
        0 => KeyState::Released,
        1 => KeyState::Pressed,
        2 => KeyState::Repeated,
        _ => return None,
    };
    Some(Event::Key {
        code: KeyCode(raw.code),
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: InputEventKind, code: impl Into<u16>, value: i32) -> RawEvent {
        RawEvent {
            event_type: kind.into(),
            code: code.into(),
            value,
        }
    }

    fn batch_end() -> RawEvent {
        RawEvent::sync(SyncCode::BatchEnd, 0)
    }

    fn decode_all(decoder: &mut Decoder, raws: &[RawEvent]) -> Vec<Event> {
        let mut out = Vec::new();
        for &raw in raws {
            decoder.decode(raw, &mut out);
        }
        out
    }

    #[test]
    fn batch_gives_motion_then_buttons_and_keys() {
        let mut decoder = Decoder::new();
        let events = decode_all(
            &mut decoder,
            &[
                raw(InputEventKind::Key, Button::Left, 1),
                raw(InputEventKind::Motion, MotionAxis::X, 3),
                raw(InputEventKind::Motion, MotionAxis::Y, -2),
                raw(InputEventKind::Motion, MotionAxis::X, 4),
                raw(InputEventKind::Key, KeyCode::ENTER.0, 1),
            ],
        );
        assert_eq!(events, [], "nothing before the batch ends");
        let events = decode_all(&mut decoder, &[batch_end()]);
        assert_eq!(
            events,
            [
                Event::Motion { dx: 7, dy: -2 },
                Event::Button {
                    button: MouseButton::Left,
                    state: ButtonState::Pressed,
                },
                Event::Key {
                    code: KeyCode::ENTER,
                    state: KeyState::Pressed,
                },
            ]
        );
    }

    #[test]
    fn key_states_and_button_release() {
        let mut decoder = Decoder::new();
        let events = decode_all(
            &mut decoder,
            &[
                raw(InputEventKind::Key, 30u16, 1),
                raw(InputEventKind::Key, 30u16, 2),
                raw(InputEventKind::Key, 30u16, 0),
                raw(InputEventKind::Key, Button::Right, 0),
                raw(InputEventKind::Key, Button::Right, 2),
                batch_end(),
            ],
        );
        let a = |state| Event::Key {
            code: KeyCode(30),
            state,
        };
        assert_eq!(
            events,
            [
                a(KeyState::Pressed),
                a(KeyState::Repeated),
                a(KeyState::Released),
                Event::Button {
                    button: MouseButton::Right,
                    state: ButtonState::Released,
                },
            ]
        );
    }

    #[test]
    fn position_keeps_the_axis_that_does_not_change() {
        let mut decoder = Decoder::new();
        let events = decode_all(
            &mut decoder,
            &[
                raw(InputEventKind::Position, PositionAxis::X, 10),
                batch_end(),
                raw(InputEventKind::Position, PositionAxis::Y, 20),
                batch_end(),
                raw(InputEventKind::Position, PositionAxis::X, 11),
                batch_end(),
            ],
        );
        assert_eq!(
            events,
            [
                Event::Position { x: 10, y: 20 },
                Event::Position { x: 11, y: 20 },
            ]
        );
    }

    #[test]
    fn lost_events_discard_the_partial_batch() {
        let mut decoder = Decoder::new();
        let events = decode_all(
            &mut decoder,
            &[
                raw(InputEventKind::Motion, MotionAxis::X, 5),
                RawEvent::sync(SyncCode::EventsLost, 9),
                raw(InputEventKind::Motion, MotionAxis::Wheel, -1),
                batch_end(),
            ],
        );
        assert_eq!(events, [Event::Wheel { delta: -1 }]);
    }
}
