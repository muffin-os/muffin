//! Wire structs and constants of the virtio-gpu control queue.
//!
//! Sources: OASIS Virtio 1.3 section 5.7 and `virgl_hw.h` of virglrenderer.
//! x86 is little-endian, so the `le32`/`le64` fields are plain `u32`/`u64`.

use core::mem::size_of;
use core::{ptr, slice};

use bitflags::bitflags;

pub const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const CMD_SET_SCANOUT: u32 = 0x0103;
pub const CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
pub const CMD_CTX_CREATE: u32 = 0x0200;
pub const CMD_CTX_DESTROY: u32 = 0x0201;
pub const CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
pub const CMD_CTX_DETACH_RESOURCE: u32 = 0x0203;
pub const CMD_RESOURCE_CREATE_3D: u32 = 0x0204;
pub const CMD_TRANSFER_TO_HOST_3D: u32 = 0x0205;
pub const CMD_TRANSFER_FROM_HOST_3D: u32 = 0x0206;
pub const CMD_SUBMIT_3D: u32 = 0x0207;

pub const RESP_OK_NODATA: u32 = 0x1100;
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const RESP_ERR_FIRST: u32 = 0x1200;
pub const RESP_ERR_LAST: u32 = 0x1205;

pub const FLAG_FENCE: u32 = 1;

pub const FORMAT_B8G8R8A8_UNORM: u32 = 1;
/// `VIRGL_FORMAT_R8_UNORM`: the element format of a vertex buffer.
pub const VIRGL_FORMAT_R8_UNORM: u32 = 64;
pub const PIPE_BUFFER: u32 = 0;
pub const PIPE_TEXTURE_2D: u32 = 2;
pub const VIRGL_BIND_RENDER_TARGET: u32 = 1 << 1;
pub const VIRGL_BIND_SAMPLER_VIEW: u32 = 1 << 3;
pub const VIRGL_BIND_VERTEX_BUFFER: u32 = 1 << 4;
pub const VIRGL_BIND_SCANOUT: u32 = 1 << 18;
/// The largest dword count of one virgl command (`VIRGL_CMD0_MAX_DWORDS`).
pub const VIRGL_CMD0_MAX_DWORDS: usize = ((1 << 16) - 1) / 4 * 4;

pub const MAX_SCANOUTS: usize = 16;

bitflags! {
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    pub struct Features: u64 {
        const VIRGL = 1 << 0;
        const EDID = 1 << 1;
        const RING_INDIRECT_DESC = 1 << 28;
        const RING_EVENT_IDX = 1 << 29;
        const VERSION_1 = 1 << 32;
    }
}

/// A plain `#[repr(C)]` struct that the driver copies to and from the queue buffers.
///
/// # Safety
/// The type must be `#[repr(C)]`, must contain only `u8`, `u32`, `u64` fields and
/// arrays or other `Wire` types, and must have no padding bytes. Every bit pattern
/// must be a valid value.
pub unsafe trait Wire: Copy {
    fn as_bytes(&self) -> &[u8] {
        // SAFETY: `Self` has no padding bytes and lives for the borrow.
        unsafe { slice::from_raw_parts(ptr::from_ref(self).cast::<u8>(), size_of::<Self>()) }
    }

    /// Reads a value from the start of `bytes`. Panics when `bytes` is too short.
    fn read_from(bytes: &[u8]) -> Self {
        assert!(bytes.len() >= size_of::<Self>());
        // SAFETY: the length check holds and every bit pattern is a valid `Self`.
        unsafe { ptr::read_unaligned(bytes.as_ptr().cast::<Self>()) }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CtrlHeader {
    pub type_: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub ring_idx: u8,
    pub padding: [u8; 3],
}

impl CtrlHeader {
    pub const fn new(type_: u32) -> Self {
        Self {
            type_,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            ring_idx: 0,
            padding: [0; 3],
        }
    }

    pub const fn with_ctx(type_: u32, ctx: ContextId) -> Self {
        let mut hdr = Self::new(type_);
        hdr.ctx_id = ctx.0;
        hdr
    }
}

/// A virgl rendering context. Exists only on a device with 3D support. The
/// driver numbers contexts from 1, because the device reads 0 as "no context".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContextId(u32);

impl ContextId {
    pub const FIRST: Self = Self(1);

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// A device resource, 2D or 3D. The driver numbers resources from 1, because
/// the device reads 0 as "no resource".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResourceId(u32);

impl ResourceId {
    /// The 2D resource that backs `/dev/fb0`.
    pub const FRAMEBUFFER: Self = Self(1);

    /// Wraps a number from the ioctl ABI. Only a lookup tells whether it names
    /// a live resource.
    pub const fn from_abi(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn to_abi(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    pub const fn to_wire(self) -> u32 {
        self.0
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Box3d {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdResourceCreate2d {
    pub hdr: CtrlHeader,
    pub resource_id: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

/// `RESOURCE_UNREF`, `RESOURCE_DETACH_BACKING`, `CTX_ATTACH_RESOURCE` and
/// `CTX_DETACH_RESOURCE` share this layout.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdResource {
    pub hdr: CtrlHeader,
    pub resource_id: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdSetScanout {
    pub hdr: CtrlHeader,
    pub rect: Rect,
    pub scanout_id: u32,
    pub resource_id: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdResourceFlush {
    pub hdr: CtrlHeader,
    pub rect: Rect,
    pub resource_id: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdTransferToHost2d {
    pub hdr: CtrlHeader,
    pub rect: Rect,
    pub offset: u64,
    pub resource_id: u32,
    pub padding: u32,
}

/// One `MemEntry` for each `nr_entries` follows this struct in the request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdResourceAttachBacking {
    pub hdr: CtrlHeader,
    pub resource_id: u32,
    pub nr_entries: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DisplayOne {
    pub rect: Rect,
    pub enabled: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RespDisplayInfo {
    pub hdr: CtrlHeader,
    pub pmodes: [DisplayOne; MAX_SCANOUTS],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdCtxCreate {
    pub hdr: CtrlHeader,
    pub nlen: u32,
    pub context_init: u32,
    pub debug_name: [u8; 64],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdResourceCreate3d {
    pub hdr: CtrlHeader,
    pub resource_id: u32,
    pub target: u32,
    pub format: u32,
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub nr_samples: u32,
    pub flags: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdTransferHost3d {
    pub hdr: CtrlHeader,
    pub box_: Box3d,
    pub offset: u64,
    pub resource_id: u32,
    pub level: u32,
    pub stride: u32,
    pub layer_stride: u32,
}

/// `size` bytes of virgl command dwords follow this struct in the request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CmdSubmit3d {
    pub hdr: CtrlHeader,
    pub size: u32,
    pub padding: u32,
}

// SAFETY: every struct below is `#[repr(C)]`, holds only `u8`/`u32`/`u64`
// fields in an order without padding, and accepts every bit pattern.
unsafe impl Wire for CtrlHeader {}
unsafe impl Wire for Rect {}
unsafe impl Wire for Box3d {}
unsafe impl Wire for CmdResourceCreate2d {}
unsafe impl Wire for CmdResource {}
unsafe impl Wire for CmdSetScanout {}
unsafe impl Wire for CmdResourceFlush {}
unsafe impl Wire for CmdTransferToHost2d {}
unsafe impl Wire for CmdResourceAttachBacking {}
unsafe impl Wire for MemEntry {}
unsafe impl Wire for DisplayOne {}
unsafe impl Wire for RespDisplayInfo {}
unsafe impl Wire for CmdCtxCreate {}
unsafe impl Wire for CmdResourceCreate3d {}
unsafe impl Wire for CmdTransferHost3d {}
unsafe impl Wire for CmdSubmit3d {}

const _: () = {
    assert!(size_of::<CtrlHeader>() == 24);
    assert!(size_of::<CmdTransferToHost2d>() == 56);
    assert!(size_of::<CmdTransferHost3d>() == 72);
    assert!(size_of::<CmdResourceCreate3d>() == 72);
    assert!(size_of::<CmdCtxCreate>() == 96);
    assert!(size_of::<CmdSubmit3d>() == 32);
    assert!(size_of::<RespDisplayInfo>() == 24 + 24 * MAX_SCANOUTS);
};
