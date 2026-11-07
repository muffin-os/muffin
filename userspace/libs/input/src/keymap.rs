use crate::{Event, KeyCode, KeyState};

/// The character that [`Keyboard::apply`] gives for the Enter key.
pub const ENTER: char = '\n';
/// The character that [`Keyboard::apply`] gives for the Backspace key.
pub const BACKSPACE: char = '\u{8}';
/// The character that [`Keyboard::apply`] gives for the Tab key.
pub const TAB: char = '\t';

/// The state of a keyboard with the US layout. It tracks the Shift keys and
/// Caps Lock, and turns key presses into characters.
#[derive(Debug, Default, Clone)]
pub struct Keyboard {
    is_left_shift_down: bool,
    is_right_shift_down: bool,
    is_caps_lock_on: bool,
}

/// The two characters of a key without and with Shift.
enum Glyph {
    /// Shift and Caps Lock select the case.
    Letter(char),
    /// Only Shift selects the character.
    Symbol(char, char),
}

impl Keyboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the modifier state for a key event. For a press or a repeat
    /// of a key with a character, returns the character. Enter gives
    /// [`ENTER`], Backspace gives [`BACKSPACE`], and Tab gives [`TAB`]. All
    /// other events, and the keypad, give `None`.
    pub fn apply(&mut self, event: Event) -> Option<char> {
        let Event::Key { code, state } = event else {
            return None;
        };
        let is_down = state != KeyState::Released;
        match code {
            KeyCode::LEFT_SHIFT => self.is_left_shift_down = is_down,
            KeyCode::RIGHT_SHIFT => self.is_right_shift_down = is_down,
            KeyCode::CAPS_LOCK if state == KeyState::Pressed => {
                self.is_caps_lock_on = !self.is_caps_lock_on;
            }
            _ if is_down => return self.char_of(code),
            _ => {}
        }
        None
    }

    pub fn is_shift_down(&self) -> bool {
        self.is_left_shift_down || self.is_right_shift_down
    }

    pub fn is_caps_lock_on(&self) -> bool {
        self.is_caps_lock_on
    }

    fn char_of(&self, code: KeyCode) -> Option<char> {
        Some(match us_glyph(code)? {
            Glyph::Letter(c) if self.is_shift_down() != self.is_caps_lock_on => {
                c.to_ascii_uppercase()
            }
            Glyph::Letter(c) => c,
            Glyph::Symbol(_, shifted) if self.is_shift_down() => shifted,
            Glyph::Symbol(base, _) => base,
        })
    }
}

fn us_glyph(code: KeyCode) -> Option<Glyph> {
    use Glyph::{Letter, Symbol};

    const LETTER_ROWS: [(u16, &str); 3] = [(16, "qwertyuiop"), (30, "asdfghjkl"), (44, "zxcvbnm")];
    for (first, letters) in LETTER_ROWS {
        if let Some(offset) = code.0.checked_sub(first)
            && let Some(&letter) = letters.as_bytes().get(usize::from(offset))
        {
            return Some(Letter(char::from(letter)));
        }
    }
    // The digit row runs from code 2 for '1' to code 11 for '0'.
    if let Some(offset) = code.0.checked_sub(2)
        && let Some(&digit) = b"1234567890".get(usize::from(offset))
    {
        let shifted = b"!@#$%^&*()"[usize::from(offset)];
        return Some(Symbol(char::from(digit), char::from(shifted)));
    }
    let (base, shifted) = match code.0 {
        12 => ('-', '_'),
        13 => ('=', '+'),
        14 => (BACKSPACE, BACKSPACE),
        15 => (TAB, TAB),
        26 => ('[', '{'),
        27 => (']', '}'),
        28 => (ENTER, ENTER),
        39 => (';', ':'),
        40 => ('\'', '"'),
        41 => ('`', '~'),
        43 => ('\\', '|'),
        51 => (',', '<'),
        52 => ('.', '>'),
        53 => ('/', '?'),
        57 => (' ', ' '),
        _ => return None,
    };
    Some(Symbol(base, shifted))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: KeyCode = KeyCode(30);
    const KEY_1: KeyCode = KeyCode(2);
    const KEY_SLASH: KeyCode = KeyCode(53);

    fn key(code: KeyCode, state: KeyState) -> Event {
        Event::Key { code, state }
    }

    fn press(keyboard: &mut Keyboard, code: KeyCode) -> Option<char> {
        keyboard.apply(key(code, KeyState::Pressed))
    }

    #[test]
    fn shift_selects_the_upper_character() {
        let mut keyboard = Keyboard::new();
        assert_eq!(press(&mut keyboard, KEY_A), Some('a'));
        assert_eq!(press(&mut keyboard, KEY_1), Some('1'));

        assert_eq!(press(&mut keyboard, KeyCode::RIGHT_SHIFT), None);
        assert_eq!(press(&mut keyboard, KEY_A), Some('A'));
        assert_eq!(press(&mut keyboard, KEY_1), Some('!'));
        assert_eq!(press(&mut keyboard, KEY_SLASH), Some('?'));
        assert_eq!(press(&mut keyboard, KeyCode::ENTER), Some(ENTER));

        keyboard.apply(key(KeyCode::RIGHT_SHIFT, KeyState::Released));
        assert_eq!(press(&mut keyboard, KEY_A), Some('a'));
    }

    #[test]
    fn caps_lock_inverts_only_the_case_of_letters() {
        let mut keyboard = Keyboard::new();
        assert_eq!(press(&mut keyboard, KeyCode::CAPS_LOCK), None);
        keyboard.apply(key(KeyCode::CAPS_LOCK, KeyState::Repeated));
        keyboard.apply(key(KeyCode::CAPS_LOCK, KeyState::Released));
        assert_eq!(press(&mut keyboard, KEY_A), Some('A'));
        assert_eq!(press(&mut keyboard, KEY_1), Some('1'));

        press(&mut keyboard, KeyCode::LEFT_SHIFT);
        assert_eq!(press(&mut keyboard, KEY_A), Some('a'));
    }

    #[test]
    fn repeat_types_and_release_does_not() {
        let mut keyboard = Keyboard::new();
        assert_eq!(
            keyboard.apply(key(KeyCode::BACKSPACE, KeyState::Repeated)),
            Some(BACKSPACE)
        );
        assert_eq!(
            keyboard.apply(key(KeyCode::BACKSPACE, KeyState::Released)),
            None
        );
    }
}
