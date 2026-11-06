//! The event stream and the ioctl requests of the devices under `/dev/input`.
//!
//! The event layout and the numeric codes follow OASIS VIRTIO 1.2 section
//! 5.8, the virtio input device. The numeric values are the same as in the
//! BSD-2-Clause header `sys/dev/evdev/input-event-codes.h` of FreeBSD.

use core::fmt;

use crate::{EINVAL, Errno};

/// The `event_type` field of an [`InputEvent`].
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEventKind {
    /// Ends one batch of events that belong to the same moment.
    Sync = 0x00,
    /// A key or a [`Button`]. The value is 1 for pressed, 0 for released,
    /// and 2 for a key repeat.
    Key = 0x01,
    /// Movement along a [`MotionAxis`]. The value is the delta since the
    /// previous event.
    Motion = 0x02,
    /// A coordinate on a [`PositionAxis`].
    Position = 0x03,
}

impl From<InputEventKind> for u16 {
    fn from(kind: InputEventKind) -> Self {
        kind as u16
    }
}

impl TryFrom<u16> for InputEventKind {
    type Error = Errno;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Sync),
            0x01 => Ok(Self::Key),
            0x02 => Ok(Self::Motion),
            0x03 => Ok(Self::Position),
            _ => Err(EINVAL),
        }
    }
}

/// The code of an [`InputEventKind::Sync`] event. The kernel owns these
/// codes. A driver maps the sync codes of its device to them.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncCode {
    /// This code ends a batch of events that occur at the same time. The value
    /// is 0.
    BatchEnd = 0,
    /// The queue of the reader becomes full and discards whole batches. The
    /// value is the number of events that the queue discards before the reader
    /// reads this event. The events after it are complete batches. A button can
    /// change in a lost batch, so the reader must read the device state again.
    EventsLost = 1,
}

impl From<SyncCode> for u16 {
    fn from(code: SyncCode) -> Self {
        code as u16
    }
}

impl TryFrom<u16> for SyncCode {
    type Error = Errno;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::BatchEnd),
            1 => Ok(Self::EventsLost),
            _ => Err(EINVAL),
        }
    }
}

/// The code of a [`InputEventKind::Motion`] event.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionAxis {
    X = 0x00,
    Y = 0x01,
    Wheel = 0x08,
}

impl From<MotionAxis> for u16 {
    fn from(axis: MotionAxis) -> Self {
        axis as u16
    }
}

impl TryFrom<u16> for MotionAxis {
    type Error = Errno;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::X),
            0x01 => Ok(Self::Y),
            0x08 => Ok(Self::Wheel),
            _ => Err(EINVAL),
        }
    }
}

/// The code of a [`InputEventKind::Position`] event.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionAxis {
    X = 0x00,
    Y = 0x01,
}

impl From<PositionAxis> for u16 {
    fn from(axis: PositionAxis) -> Self {
        axis as u16
    }
}

impl TryFrom<u16> for PositionAxis {
    type Error = Errno;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::X),
            0x01 => Ok(Self::Y),
            _ => Err(EINVAL),
        }
    }
}

/// A pointer button, one of the codes of a [`InputEventKind::Key`] event.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left = 0x110,
    Right = 0x111,
    Middle = 0x112,
}

impl From<Button> for u16 {
    fn from(button: Button) -> Self {
        button as u16
    }
}

impl TryFrom<u16> for Button {
    type Error = Errno;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0x110 => Ok(Self::Left),
            0x111 => Ok(Self::Right),
            0x112 => Ok(Self::Middle),
            _ => Err(EINVAL),
        }
    }
}

/// One event of an input device. A read on `/dev/input/eventN` returns a
/// whole number of these.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputEvent {
    /// The raw event type. Unknown types pass through from the device.
    pub event_type: u16,
    /// The code inside the type, for example a key number or an axis.
    pub code: u16,
    pub value: i32,
}

impl InputEvent {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn sync(code: SyncCode, value: i32) -> Self {
        Self {
            event_type: InputEventKind::Sync.into(),
            code: code.into(),
            value,
        }
    }

    /// Returns `None` when the kernel does not know the event type.
    #[must_use]
    pub fn kind(self) -> Option<InputEventKind> {
        InputEventKind::try_from(self.event_type).ok()
    }

    #[must_use]
    pub fn is_sync(self) -> bool {
        self.kind() == Some(InputEventKind::Sync)
    }

    #[must_use]
    pub fn matches(self, kind: InputEventKind, code: impl Into<u16>) -> bool {
        self.kind() == Some(kind) && self.code == code.into()
    }

    /// Returns the axis of a motion event. Returns `None` for other events and
    /// for an unknown axis.
    #[must_use]
    pub fn motion_axis(self) -> Option<MotionAxis> {
        (self.kind() == Some(InputEventKind::Motion))
            .then(|| MotionAxis::try_from(self.code).ok())
            .flatten()
    }

    /// Returns the button of a key event. Returns `None` for other events and for
    /// a key that is not a pointer button.
    #[must_use]
    pub fn button(self) -> Option<Button> {
        (self.kind() == Some(InputEventKind::Key))
            .then(|| Button::try_from(self.code).ok())
            .flatten()
    }

    /// Returns true when a key event reports that the key is down. A key repeat
    /// also reports that the key is down.
    #[must_use]
    pub fn is_pressed(self) -> bool {
        self.value != 0
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..2].copy_from_slice(&self.event_type.to_ne_bytes());
        bytes[2..4].copy_from_slice(&self.code.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.value.to_ne_bytes());
        bytes
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Self {
        Self {
            event_type: u16::from_ne_bytes([bytes[0], bytes[1]]),
            code: u16::from_ne_bytes([bytes[2], bytes[3]]),
            value: i32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }
}

/// Writes the event as `<kind> <code> <value>`, for example `motion X 7` or
/// `button Left 1`. A code without a name prints as a number, and an unknown
/// type prints as `type=<n> code=<n> <value>`.
impl fmt::Display for InputEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            event_type,
            code,
            value,
        } = *self;
        match self.kind() {
            None => write!(f, "type={event_type} code={code} {value}"),
            Some(InputEventKind::Sync) => match SyncCode::try_from(code) {
                Ok(sync) => write!(f, "sync {sync:?} {value}"),
                Err(_) => write!(f, "sync code={code} {value}"),
            },
            Some(InputEventKind::Key) => match Button::try_from(code) {
                Ok(button) => write!(f, "button {button:?} {value}"),
                Err(_) => write!(f, "key {code} {value}"),
            },
            Some(InputEventKind::Motion) => match MotionAxis::try_from(code) {
                Ok(axis) => write!(f, "motion {axis:?} {value}"),
                Err(_) => write!(f, "motion code={code} {value}"),
            },
            Some(InputEventKind::Position) => match PositionAxis::try_from(code) {
                Ok(axis) => write!(f, "position {axis:?} {value}"),
                Err(_) => write!(f, "position code={code} {value}"),
            },
        }
    }
}

/// The longest device name that [`InputDeviceInfo`] carries, in bytes.
pub const INPUT_NAME_MAX_BYTES: usize = 64;

/// The identity of an input device. The kernel writes all fields.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputDeviceInfo {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    /// The number of valid bytes at the front of `name`.
    pub name_len_bytes: u32,
    /// The device name in UTF-8, cut at `INPUT_NAME_MAX_BYTES`.
    pub name: [u8; INPUT_NAME_MAX_BYTES],
}

impl Default for InputDeviceInfo {
    fn default() -> Self {
        Self {
            bustype: 0,
            vendor: 0,
            product: 0,
            version: 0,
            name_len_bytes: 0,
            name: [0; INPUT_NAME_MAX_BYTES],
        }
    }
}

impl InputDeviceInfo {
    pub const SIZE: usize = size_of::<Self>();

    /// Stores the longest prefix of `name` that fits and ends on a character boundary.
    pub fn set_name(&mut self, name: &str) {
        let mut len = name.len().min(INPUT_NAME_MAX_BYTES);
        while !name.is_char_boundary(len) {
            len -= 1;
        }
        self.name[..len].copy_from_slice(&name.as_bytes()[..len]);
        self.name_len_bytes = len as u32;
    }

    /// Returns `None` when the name bytes are not valid UTF-8.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        let len = (self.name_len_bytes as usize).min(INPUT_NAME_MAX_BYTES);
        core::str::from_utf8(&self.name[..len]).ok()
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..2].copy_from_slice(&self.bustype.to_ne_bytes());
        bytes[2..4].copy_from_slice(&self.vendor.to_ne_bytes());
        bytes[4..6].copy_from_slice(&self.product.to_ne_bytes());
        bytes[6..8].copy_from_slice(&self.version.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.name_len_bytes.to_ne_bytes());
        bytes[12..].copy_from_slice(&self.name);
        bytes
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Self {
        let mut name = [0u8; INPUT_NAME_MAX_BYTES];
        name.copy_from_slice(&bytes[12..]);
        Self {
            bustype: u16::from_ne_bytes([bytes[0], bytes[1]]),
            vendor: u16::from_ne_bytes([bytes[2], bytes[3]]),
            product: u16::from_ne_bytes([bytes[4], bytes[5]]),
            version: u16::from_ne_bytes([bytes[6], bytes[7]]),
            name_len_bytes: u32::from_ne_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            name,
        }
    }
}

/// The longest code bitmap that [`InputSupportedCodes`] carries, in bytes.
// Equals the size of the virtio input config data area, VIRTIO 1.2 section 5.8.
pub const INPUT_SUPPORTED_CODES_MAX_BYTES: usize = 128;

/// The codes an input device can send for one event type. The caller sets
/// `event_type`; the kernel writes `bits_len_bytes` and `bits`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputSupportedCodes {
    pub event_type: u16,
    /// The number of valid bytes at the front of `bits`. Zero means the device
    /// does not send this event type.
    pub bits_len_bytes: u16,
    /// Bit `code` is set when the device can send `code`.
    pub bits: [u8; INPUT_SUPPORTED_CODES_MAX_BYTES],
}

impl Default for InputSupportedCodes {
    fn default() -> Self {
        Self {
            event_type: 0,
            bits_len_bytes: 0,
            bits: [0; INPUT_SUPPORTED_CODES_MAX_BYTES],
        }
    }
}

impl InputSupportedCodes {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn for_kind(kind: InputEventKind) -> Self {
        Self {
            event_type: kind.into(),
            ..Self::default()
        }
    }

    /// Stores the longest prefix of `supported` that fits and drops the rest.
    pub fn set_bits(&mut self, supported: &[u8]) {
        let len = supported.len().min(INPUT_SUPPORTED_CODES_MAX_BYTES);
        self.bits = [0; INPUT_SUPPORTED_CODES_MAX_BYTES];
        self.bits[..len].copy_from_slice(&supported[..len]);
        self.bits_len_bytes = len as u16;
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bits_len_bytes == 0
    }

    #[must_use]
    pub fn contains(&self, code: impl Into<u16>) -> bool {
        let code = code.into();
        let byte = usize::from(code / 8);
        byte < usize::from(self.bits_len_bytes).min(INPUT_SUPPORTED_CODES_MAX_BYTES)
            && self.bits[byte] & (1 << (code % 8)) != 0
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..2].copy_from_slice(&self.event_type.to_ne_bytes());
        bytes[2..4].copy_from_slice(&self.bits_len_bytes.to_ne_bytes());
        bytes[4..].copy_from_slice(&self.bits);
        bytes
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Self {
        let mut bits = [0u8; INPUT_SUPPORTED_CODES_MAX_BYTES];
        bits.copy_from_slice(&bytes[4..]);
        Self {
            event_type: u16::from_ne_bytes([bytes[0], bytes[1]]),
            bits_len_bytes: u16::from_ne_bytes([bytes[2], bytes[3]]),
            bits,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::format;
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn event_bytes_round_trip_keeps_negative_motion() {
        let event = InputEvent {
            event_type: InputEventKind::Motion.into(),
            code: MotionAxis::X.into(),
            value: -7,
        };
        assert_eq!(InputEvent::from_bytes(event.to_bytes()), event);
        assert_eq!(event.kind(), Some(InputEventKind::Motion));
        assert!(event.matches(InputEventKind::Motion, MotionAxis::X));
        assert!(!event.matches(InputEventKind::Motion, MotionAxis::Y));
        assert!(!event.is_sync());
        assert_eq!(event.to_string(), "motion X -7");
    }

    #[test]
    fn unknown_event_type_has_no_kind() {
        let event = InputEvent {
            event_type: 0x15,
            code: 3,
            value: 0,
        };
        assert_eq!(event.kind(), None);
        assert_eq!(event.to_string(), "type=21 code=3 0");
    }

    #[test]
    fn key_event_names_a_button_and_numbers_a_key() {
        let button = InputEvent {
            event_type: InputEventKind::Key.into(),
            code: Button::Left.into(),
            value: 1,
        };
        assert_eq!(button.to_string(), "button Left 1");
        let key = InputEvent {
            event_type: InputEventKind::Key.into(),
            code: 30,
            value: 0,
        };
        assert_eq!(key.to_string(), "key 30 0");
    }

    #[test]
    fn device_name_is_cut_on_a_char_boundary() {
        let mut info = InputDeviceInfo::default();
        let long = "é".repeat(INPUT_NAME_MAX_BYTES);
        info.set_name(&long);
        assert_eq!(info.name_len_bytes as usize, INPUT_NAME_MAX_BYTES);
        assert_eq!(info.name(), Some(&long[..INPUT_NAME_MAX_BYTES]));

        let odd = format!("{}é", "a".repeat(INPUT_NAME_MAX_BYTES - 1));
        info.set_name(&odd);
        assert_eq!(info.name_len_bytes as usize, INPUT_NAME_MAX_BYTES - 1);
        assert_eq!(info.name(), Some(&odd[..INPUT_NAME_MAX_BYTES - 1]));
        assert_eq!(InputDeviceInfo::from_bytes(info.to_bytes()), info);
    }

    #[test]
    fn supported_codes_only_count_valid_bytes() {
        let mut codes = InputSupportedCodes::for_kind(InputEventKind::Key);
        assert!(codes.is_empty());
        codes.set_bits(&[0b0000_0010]);
        assert!(!codes.is_empty());
        assert!(codes.contains(1u16));
        assert!(!codes.contains(0u16));
        assert!(
            !codes.contains(16u16),
            "a code past the valid bytes is not supported"
        );
        codes.set_bits(&[0b0000_0010, 0, 0b0000_0001]);
        assert!(codes.contains(16u16));
        assert_eq!(InputSupportedCodes::from_bytes(codes.to_bytes()), codes);
    }

    #[test]
    fn supported_codes_drop_bytes_past_the_bitmap() {
        let mut codes = InputSupportedCodes::for_kind(InputEventKind::Key);
        let too_long = [0xFF; INPUT_SUPPORTED_CODES_MAX_BYTES + 1];
        codes.set_bits(&too_long);
        assert_eq!(
            codes.bits_len_bytes as usize,
            INPUT_SUPPORTED_CODES_MAX_BYTES
        );
        assert!(codes.contains((INPUT_SUPPORTED_CODES_MAX_BYTES * 8 - 1) as u16));
        assert!(!codes.contains((INPUT_SUPPORTED_CODES_MAX_BYTES * 8) as u16));
    }
}
