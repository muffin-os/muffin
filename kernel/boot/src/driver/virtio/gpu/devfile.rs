//! The device files `/dev/fb0`, `/dev/gpu0`, and `/dev/render0`.
//!
//! Lock order: the `DevFs` lock (held by the caller) comes first, then the
//! process fd table (inside `FdLookup`, released before the device lock), then
//! the `GpuDevice` mutex. `GpuDevice` never takes the `DevFs` lock.

use alloc::sync::Arc;
use core::ptr::NonNull;
use core::slice;

use kernel_abi::{
    FbScreenInfo, GpuFenceInfo, GpuFlush, GpuImport, GpuInfo, GpuSetScanout, GpuTransfer,
    GpuWaitFence, IoctlRequest, S_IFCHR,
};
use kernel_devfs::DevFile;
use kernel_vfs::{
    FdLookup, FsyncError, IoctlError, MappedMemory, MmapError, MmapRegion, ReadError, Stat,
    StatError, WriteError,
};
use spin::Mutex;

use super::device::{GpuDevice, GpuError};
use super::wire::{ContextId, ResourceId};

/// The framebuffer bytes that `/dev/fb0` lends out for shared mappings.
#[derive(Debug)]
pub struct FbMemory {
    pub ptr: NonNull<u8>,
    pub len: usize,
}

// SAFETY: `ptr` aliases a framebuffer that lives for the kernel's lifetime,
// and every access through it is unsafe on the caller.
unsafe impl Send for FbMemory {}
unsafe impl Sync for FbMemory {}

impl MappedMemory for FbMemory {
    fn ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    fn len(&self) -> usize {
        self.len
    }
}

/// Device file behind `/dev/fb0`. Holds the GPU so `fsync` can flush, and the
/// framebuffer memory so `mmap` can lend it out.
pub struct FbDevFile {
    pub device: Arc<Mutex<GpuDevice>>,
    pub memory: Arc<FbMemory>,
    pub width: u32,
    pub height: u32,
}

impl FbDevFile {
    fn as_slice_mut(&mut self) -> &mut [u8] {
        // SAFETY: `DevFs` serializes access through `&mut self`.
        unsafe { slice::from_raw_parts_mut(self.memory.ptr.as_ptr(), self.memory.len) }
    }
}

impl DevFile for FbDevFile {
    fn read(&mut self, buf: &mut [u8], offset: usize) -> Result<usize, ReadError> {
        let fb = self.as_slice_mut();
        let offset = offset.min(fb.len());
        let n = buf.len().min(fb.len() - offset);
        buf[..n].copy_from_slice(&fb[offset..offset + n]);
        Ok(n)
    }

    fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        let fb = self.as_slice_mut();
        if offset >= fb.len() {
            return Err(WriteError::WriteFailed);
        }
        let n = buf.len().min(fb.len() - offset);
        fb[offset..offset + n].copy_from_slice(&buf[..n]);
        Ok(n)
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = self.memory.len;
        stat.mode = S_IFCHR | 0o666;
        Ok(())
    }

    fn mmap(&mut self) -> Result<MmapRegion, MmapError> {
        Ok(self.memory.clone())
    }

    fn fsync(&mut self) -> Result<(), FsyncError> {
        self.device
            .lock()
            .flush_framebuffer()
            .map_err(|_| FsyncError::Failed)
    }

    fn ioctl(
        &mut self,
        request: IoctlRequest,
        arg: &mut [u8],
        _lookup: &dyn FdLookup,
    ) -> Result<usize, IoctlError> {
        match request {
            IoctlRequest::FbGetScreenInfo => {
                let Ok(arg) = <&mut [u8; FbScreenInfo::SIZE]>::try_from(arg) else {
                    return Err(IoctlError::InvalidArgument);
                };
                let info = FbScreenInfo {
                    width: self.width,
                    height: self.height,
                    pitch: self.width * 4,
                    bpp: 32,
                };
                *arg = info.to_bytes();
                Ok(0)
            }
            IoctlRequest::GpuGetInfo
            | IoctlRequest::GpuImport
            | IoctlRequest::GpuTransfer
            | IoctlRequest::GpuFenceInfo
            | IoctlRequest::GpuWaitFence
            | IoctlRequest::GpuSetScanout
            | IoctlRequest::GpuFlush
            | IoctlRequest::InputGetInfo
            | IoctlRequest::InputGetSupportedCodes => Err(IoctlError::NotSupported),
        }
    }
}

/// `Display` is `/dev/gpu0`; it may change and flush the display. `Render` is
/// `/dev/render0`; it renders into imported resources only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuNode {
    Display,
    Render,
}

/// Device file behind `/dev/gpu0` and `/dev/render0`. Each open file owns one
/// virgl context; a device without 3D support gives none.
pub struct GpuDevFile {
    device: Arc<Mutex<GpuDevice>>,
    ctx: Option<ContextId>,
    node: GpuNode,
}

impl GpuDevFile {
    pub fn open(device: Arc<Mutex<GpuDevice>>, node: GpuNode) -> Result<Self, GpuError> {
        let ctx = {
            let mut guard = device.lock();
            guard.has_3d().then(|| guard.create_context()).transpose()?
        };
        Ok(Self { device, ctx, node })
    }

    fn context(&self) -> Result<ContextId, IoctlError> {
        self.ctx.ok_or(IoctlError::NotSupported)
    }
}

impl Drop for GpuDevFile {
    fn drop(&mut self) {
        if let Some(ctx) = self.ctx {
            // The context is gone either way; a device error leaves nothing to undo.
            let _ = self.device.lock().destroy_context(ctx);
        }
    }
}

fn ioctl_error(err: GpuError) -> IoctlError {
    match err {
        GpuError::No3d => IoctlError::NotSupported,
        GpuError::DisplayOwned => IoctlError::Busy,
        GpuError::InvalidArgument | GpuError::Device(_) | GpuError::Queue(_) => {
            IoctlError::InvalidArgument
        }
    }
}

/// Decodes a fixed-size ioctl argument.
fn decode<const N: usize, T>(
    arg: &[u8],
    from_bytes: impl FnOnce([u8; N]) -> Option<T>,
) -> Result<T, IoctlError> {
    let bytes = <[u8; N]>::try_from(arg).map_err(|_| IoctlError::InvalidArgument)?;
    from_bytes(bytes).ok_or(IoctlError::InvalidArgument)
}

/// Writes a fixed-size ioctl result.
fn encode<const N: usize>(arg: &mut [u8], bytes: [u8; N]) -> Result<usize, IoctlError> {
    let out = <&mut [u8; N]>::try_from(arg).map_err(|_| IoctlError::InvalidArgument)?;
    *out = bytes;
    Ok(0)
}

impl DevFile for GpuDevFile {
    fn read(&mut self, _buf: &mut [u8], _offset: usize) -> Result<usize, ReadError> {
        Ok(0)
    }

    /// Submits `buf` as one virgl command stream. `sys_write` made `buf`
    /// resident, so the device lock never faults on user memory.
    fn write(&mut self, buf: &[u8], _offset: usize) -> Result<usize, WriteError> {
        let Some(ctx) = self.ctx else {
            return Err(WriteError::NotWritable);
        };
        match self.device.lock().submit(ctx, buf) {
            Ok(_) => Ok(buf.len()),
            Err(_) => Err(WriteError::WriteFailed),
        }
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = 0;
        stat.mode = S_IFCHR | 0o666;
        Ok(())
    }

    fn ioctl(
        &mut self,
        request: IoctlRequest,
        arg: &mut [u8],
        lookup: &dyn FdLookup,
    ) -> Result<usize, IoctlError> {
        if self.node == GpuNode::Render
            && matches!(
                request,
                IoctlRequest::GpuSetScanout | IoctlRequest::GpuFlush
            )
        {
            return Err(IoctlError::NotSupported);
        }
        match request {
            IoctlRequest::GpuGetInfo => {
                let device = self.device.lock();
                let scanout = device.scanout();
                let info = GpuInfo {
                    has_3d: u32::from(device.has_3d()),
                    width: scanout.width,
                    height: scanout.height,
                };
                drop(device);
                encode(arg, info.to_bytes())
            }
            IoctlRequest::GpuImport => {
                let mut import = decode(arg, GpuImport::from_bytes)?;
                let ctx = self.context()?;
                let region = lookup
                    .mmap_region(import.fd)
                    .map_err(|_| IoctlError::InvalidArgument)?;
                import.resource = self
                    .device
                    .lock()
                    .import(ctx, region, import.kind, import.width, import.height)
                    .map_err(ioctl_error)?
                    .to_abi();
                encode(arg, import.to_bytes())
            }
            IoctlRequest::GpuTransfer => {
                let transfer = decode(arg, GpuTransfer::from_bytes)?;
                self.device
                    .lock()
                    .transfer(self.context()?, transfer)
                    .map_err(ioctl_error)?;
                Ok(0)
            }
            IoctlRequest::GpuFenceInfo => {
                let device = self.device.lock();
                let info = GpuFenceInfo {
                    last_submitted: device.last_submitted(),
                    last_completed: device.last_completed(),
                };
                drop(device);
                encode(arg, info.to_bytes())
            }
            IoctlRequest::GpuWaitFence => {
                let wait = decode(arg, GpuWaitFence::from_bytes)?;
                self.device
                    .lock()
                    .wait_fence(wait.fence)
                    .map_err(ioctl_error)?;
                Ok(0)
            }
            IoctlRequest::GpuSetScanout => {
                let scanout = decode(arg, GpuSetScanout::from_bytes)?;
                self.device
                    .lock()
                    .set_scanout(self.context()?, ResourceId::from_abi(scanout.resource))
                    .map_err(ioctl_error)?;
                Ok(0)
            }
            IoctlRequest::GpuFlush => {
                let flush = decode(arg, GpuFlush::from_bytes)?;
                self.device
                    .lock()
                    .flush(self.context()?, flush)
                    .map_err(ioctl_error)?;
                Ok(0)
            }
            IoctlRequest::FbGetScreenInfo
            | IoctlRequest::InputGetInfo
            | IoctlRequest::InputGetSupportedCodes => Err(IoctlError::NotSupported),
        }
    }
}
