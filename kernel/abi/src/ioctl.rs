use crate::{ENOTTY, Errno};

/// A device control request, the typed form of the raw request number
/// passed to the ioctl syscall.
#[repr(usize)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum IoctlRequest {
    /// Fills an [`FbScreenInfo`] with the framebuffer geometry.
    FbGetScreenInfo = 1,
}

impl IoctlRequest {
    /// Returns the size in bytes of the argument buffer this request
    /// reads and writes.
    #[must_use]
    pub const fn arg_size(self) -> usize {
        match self {
            Self::FbGetScreenInfo => FbScreenInfo::SIZE,
        }
    }

    /// Returns the raw request number for the syscall register.
    #[must_use]
    pub const fn number(self) -> usize {
        self as usize
    }
}

impl TryFrom<usize> for IoctlRequest {
    type Error = Errno;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::FbGetScreenInfo),
            _ => Err(ENOTTY),
        }
    }
}

/// Screen geometry of a framebuffer device, all fields in pixels except
/// `pitch` (bytes per scanline) and `bpp` (bits per pixel).
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct FbScreenInfo {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u32,
}

impl FbScreenInfo {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.width.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.height.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.pitch.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.bpp.to_ne_bytes());
        bytes
    }

    #[must_use]
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Self {
        Self {
            width: u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            height: u32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            pitch: u32::from_ne_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            bpp: u32::from_ne_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_request_number_round_trips() {
        let request = IoctlRequest::FbGetScreenInfo;
        assert_eq!(
            IoctlRequest::try_from(request.number()),
            Ok(request),
            "request number should round-trip through TryFrom"
        );
    }

    #[test]
    fn ioctl_request_unknown_is_enotty() {
        assert_eq!(
            IoctlRequest::try_from(999),
            Err(ENOTTY),
            "unknown request number should map to ENOTTY"
        );
    }

    #[test]
    fn fb_screen_info_bytes_round_trip() {
        let info = FbScreenInfo {
            width: 1280,
            height: 720,
            pitch: 5120,
            bpp: 32,
        };
        assert_eq!(
            FbScreenInfo::from_bytes(&info.to_bytes()),
            info,
            "screen info should survive a to_bytes/from_bytes round-trip"
        );
    }
}
