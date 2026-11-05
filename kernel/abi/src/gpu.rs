//! Argument structs of the GPU ioctl requests on `/dev/gpu0`.

use core::ffi::c_int;

use crate::{EINVAL, Errno, FdNum};

/// The kind of a GPU resource.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuResourceKind {
    #[default]
    /// A byte buffer. Its width is the byte length and its height is 1.
    Buffer = 0,
    /// A 2D texture with 4 bytes for each pixel.
    Texture = 1,
}

impl TryFrom<u32> for GpuResourceKind {
    type Error = Errno;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Buffer),
            1 => Ok(Self::Texture),
            _ => Err(EINVAL),
        }
    }
}

/// The direction of a copy between guest memory and the host resource.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuTransferDirection {
    #[default]
    /// Copies guest memory to the host resource.
    ToHost = 0,
    /// Copies the host resource to guest memory.
    FromHost = 1,
}

impl TryFrom<u32> for GpuTransferDirection {
    type Error = Errno;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::ToHost),
            1 => Ok(Self::FromHost),
            _ => Err(EINVAL),
        }
    }
}

/// The capabilities of the GPU. The kernel writes all fields.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuInfo {
    pub has_3d: u32,
    pub width: u32,
    pub height: u32,
}

impl GpuInfo {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.has_3d.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.width.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.height.to_ne_bytes());
        bytes
    }

    /// Returns `None` when a field holds an invalid enum discriminant.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Option<Self> {
        Some(Self {
            has_3d: u32::from_ne_bytes(bytes[0..4].try_into().ok()?),
            width: u32::from_ne_bytes(bytes[4..8].try_into().ok()?),
            height: u32::from_ne_bytes(bytes[8..12].try_into().ok()?),
        })
    }
}

/// Imports a mappable file of the caller as a GPU resource.
/// For a `Buffer`, `width` is the byte length and `height` is 1.
/// The kernel writes `resource` (an id of 2 or more).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuImport {
    pub fd: FdNum,
    pub kind: GpuResourceKind,
    pub width: u32,
    pub height: u32,
    pub resource: u32,
}

impl GpuImport {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&c_int::from(self.fd).to_ne_bytes());
        bytes[4..8].copy_from_slice(&(self.kind as u32).to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.width.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.height.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.resource.to_ne_bytes());
        bytes
    }

    /// Returns `None` when a field holds an invalid enum discriminant.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Option<Self> {
        Some(Self {
            fd: FdNum::from(c_int::from_ne_bytes(bytes[0..4].try_into().ok()?)),
            kind: GpuResourceKind::try_from(u32::from_ne_bytes(bytes[4..8].try_into().ok()?))
                .ok()?,
            width: u32::from_ne_bytes(bytes[8..12].try_into().ok()?),
            height: u32::from_ne_bytes(bytes[12..16].try_into().ok()?),
            resource: u32::from_ne_bytes(bytes[16..20].try_into().ok()?),
        })
    }
}

/// Copies a rectangle between guest memory and a resource.
/// For a `Buffer`, `x` is the byte offset, `width` is the byte count, `y` is 0 and `height` is 1.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuTransfer {
    pub resource: u32,
    pub direction: GpuTransferDirection,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl GpuTransfer {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.resource.to_ne_bytes());
        bytes[4..8].copy_from_slice(&(self.direction as u32).to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.x.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.y.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.width.to_ne_bytes());
        bytes[20..24].copy_from_slice(&self.height.to_ne_bytes());
        bytes
    }

    /// Returns `None` when a field holds an invalid enum discriminant.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Option<Self> {
        Some(Self {
            resource: u32::from_ne_bytes(bytes[0..4].try_into().ok()?),
            direction: GpuTransferDirection::try_from(u32::from_ne_bytes(
                bytes[4..8].try_into().ok()?,
            ))
            .ok()?,
            x: u32::from_ne_bytes(bytes[8..12].try_into().ok()?),
            y: u32::from_ne_bytes(bytes[12..16].try_into().ok()?),
            width: u32::from_ne_bytes(bytes[16..20].try_into().ok()?),
            height: u32::from_ne_bytes(bytes[20..24].try_into().ok()?),
        })
    }
}

/// The fence counters of the device. The kernel writes all fields.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuFenceInfo {
    pub last_submitted: u64,
    pub last_completed: u64,
}

impl GpuFenceInfo {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.last_submitted.to_ne_bytes());
        bytes[8..16].copy_from_slice(&self.last_completed.to_ne_bytes());
        bytes
    }

    /// Returns `None` when a field holds an invalid enum discriminant.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Option<Self> {
        Some(Self {
            last_submitted: u64::from_ne_bytes(bytes[0..8].try_into().ok()?),
            last_completed: u64::from_ne_bytes(bytes[8..16].try_into().ok()?),
        })
    }
}

/// Waits until the device completes `fence`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuWaitFence {
    pub fence: u64,
}

impl GpuWaitFence {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.fence.to_ne_bytes());
        bytes
    }

    /// Returns `None` when a field holds an invalid enum discriminant.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Option<Self> {
        Some(Self {
            fence: u64::from_ne_bytes(bytes[0..8].try_into().ok()?),
        })
    }
}

/// Shows a texture resource on the scanout.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuSetScanout {
    pub resource: u32,
}

impl GpuSetScanout {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.resource.to_ne_bytes());
        bytes
    }

    /// Returns `None` when a field holds an invalid enum discriminant.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Option<Self> {
        Some(Self {
            resource: u32::from_ne_bytes(bytes[0..4].try_into().ok()?),
        })
    }
}

/// Updates a rectangle of the scanout from a resource.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuFlush {
    pub resource: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl GpuFlush {
    pub const SIZE: usize = size_of::<Self>();

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.resource.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.x.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.y.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.width.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.height.to_ne_bytes());
        bytes
    }

    /// Returns `None` when a field holds an invalid enum discriminant.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Option<Self> {
        Some(Self {
            resource: u32::from_ne_bytes(bytes[0..4].try_into().ok()?),
            x: u32::from_ne_bytes(bytes[4..8].try_into().ok()?),
            y: u32::from_ne_bytes(bytes[8..12].try_into().ok()?),
            width: u32::from_ne_bytes(bytes[12..16].try_into().ok()?),
            height: u32::from_ne_bytes(bytes[16..20].try_into().ok()?),
        })
    }
}
