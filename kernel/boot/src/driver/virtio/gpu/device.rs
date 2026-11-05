//! The virtio-gpu device: control queue, the 2D framebuffer of `/dev/fb0`,
//! and the virgl 3D API behind `/dev/gpu0`.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::mem::size_of;
use core::ptr::NonNull;
use core::slice;

use kernel_abi::{GpuFlush, GpuResourceKind, GpuTransfer, GpuTransferDirection};
use kernel_vfs::MmapRegion;
use thiserror::Error;
use virtio_drivers::queue::VirtQueue;
use virtio_drivers::transport::Transport;
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::{BufferDirection, Hal, PAGE_SIZE};
use x86_64::VirtAddr;
use x86_64::structures::paging::{PageSize, Size4KiB};

use super::wire::*;
use crate::driver::virtio::hal::HalImpl;
use crate::mem::address_space::AddressSpace;

const CONTROL_QUEUE: u16 = 0;
const QUEUE_SIZE: usize = 2;
const SCANOUT_ID: u32 = 0;
/// One page for the request header plus the largest single virgl command.
const SUBMIT_STAGING_PAGES: usize = 1 + VIRGL_CMD0_MAX_DWORDS * 4 / PAGE_SIZE + 1;
const MAX_CHUNK_BYTES: usize = VIRGL_CMD0_MAX_DWORDS * 4;
const DEBUG_NAME: &[u8] = b"muffin";

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum GpuError {
    #[error("the device has no 3D support")]
    No3d,
    #[error("invalid argument")]
    InvalidArgument,
    #[error("the device answered with error {0:#x}")]
    Device(u32),
    #[error("{0}")]
    Queue(virtio_drivers::Error),
    #[error("another context owns the display")]
    DisplayOwned,
}

/// Physically contiguous pages that the device reads and writes.
pub struct DmaBuffer {
    paddr: u64,
    ptr: NonNull<u8>,
    pages: usize,
}

// SAFETY: the buffer owns its pages; nothing else aliases them.
unsafe impl Send for DmaBuffer {}

impl DmaBuffer {
    fn new(pages: usize) -> Self {
        let (paddr, ptr) = HalImpl::dma_alloc(pages, BufferDirection::Both);
        Self { paddr, ptr, pages }
    }

    pub fn paddr(&self) -> u64 {
        self.paddr
    }

    pub fn ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    fn len(&self) -> usize {
        self.pages * PAGE_SIZE
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: the pages are mapped and owned for the lifetime of `self`.
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len()) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` makes the access exclusive.
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len()) }
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        // SAFETY: `paddr`, `ptr` and `pages` come from `dma_alloc`.
        unsafe {
            HalImpl::dma_dealloc(self.paddr, self.ptr, self.pages);
        }
    }
}

struct ImportedResource {
    /// Pins the frames of the shared memory while the device uses them.
    _region: MmapRegion,
    /// The physical base of `region`; the key in `by_phys`.
    phys: u64,
    kind: GpuResourceKind,
    width: u32,
    height: u32,
    importers: BTreeSet<ContextId>,
}

pub struct GpuDevice {
    transport: PciTransport,
    control_queue: VirtQueue<HalImpl, QUEUE_SIZE>,
    has_3d: bool,
    scanout: Rect,
    request: DmaBuffer,
    response: DmaBuffer,
    framebuffer: Option<DmaBuffer>,
    next_resource: ResourceId,
    next_ctx: ContextId,
    last_submitted: u64,
    last_completed: u64,
    resources: BTreeMap<ResourceId, ImportedResource>,
    by_phys: BTreeMap<u64, ResourceId>,
    /// The context that called `set_scanout` first. Only it changes or
    /// flushes the display, until it drops.
    display_owner: Option<ContextId>,
}

impl GpuDevice {
    pub fn new(mut transport: PciTransport) -> Result<Self, GpuError> {
        let negotiated = transport.begin_init(
            Features::VIRGL
                | Features::EDID
                | Features::RING_INDIRECT_DESC
                | Features::RING_EVENT_IDX
                | Features::VERSION_1,
        );
        let control_queue = VirtQueue::new(
            &mut transport,
            CONTROL_QUEUE,
            negotiated.contains(Features::RING_INDIRECT_DESC),
            negotiated.contains(Features::RING_EVENT_IDX),
        )
        .map_err(GpuError::Queue)?;
        transport.finish_init();

        let mut device = Self {
            transport,
            control_queue,
            has_3d: negotiated.contains(Features::VIRGL),
            scanout: Rect::default(),
            request: DmaBuffer::new(SUBMIT_STAGING_PAGES),
            response: DmaBuffer::new(1),
            framebuffer: None,
            next_resource: ResourceId::FRAMEBUFFER.next(),
            next_ctx: ContextId::FIRST,
            last_submitted: 0,
            last_completed: 0,
            resources: BTreeMap::new(),
            by_phys: BTreeMap::new(),
            display_owner: None,
        };
        let info: RespDisplayInfo = device.request(CtrlHeader::new(CMD_GET_DISPLAY_INFO), &[])?;
        check_response(&info.hdr, RESP_OK_DISPLAY_INFO)?;
        device.scanout = info.pmodes[0].rect;
        Ok(device)
    }

    pub fn has_3d(&self) -> bool {
        self.has_3d
    }

    pub fn scanout(&self) -> Rect {
        self.scanout
    }

    pub fn last_submitted(&self) -> u64 {
        self.last_submitted
    }

    pub fn last_completed(&self) -> u64 {
        self.last_completed
    }

    /// Sends `req` followed by `trailer` and returns the response.
    fn request<Req: Wire, Rsp: Wire>(&mut self, req: Req, trailer: &[u8]) -> Result<Rsp, GpuError> {
        let len = size_of::<Req>() + trailer.len();
        let staging = self.request.as_mut_slice();
        staging[..size_of::<Req>()].copy_from_slice(req.as_bytes());
        staging[size_of::<Req>()..len].copy_from_slice(trailer);
        self.control_queue
            .add_notify_wait_pop(
                &[&self.request.as_slice()[..len]],
                &mut [self.response.as_mut_slice()],
                &mut self.transport,
            )
            .map_err(GpuError::Queue)?;
        Ok(Rsp::read_from(self.response.as_slice()))
    }

    fn request_nodata<Req: Wire>(&mut self, req: Req, trailer: &[u8]) -> Result<(), GpuError> {
        let hdr: CtrlHeader = self.request(req, trailer)?;
        check_response(&hdr, RESP_OK_NODATA)
    }

    fn resource_command(
        &mut self,
        type_: u32,
        ctx: Option<ContextId>,
        resource: ResourceId,
    ) -> Result<(), GpuError> {
        let hdr = match ctx {
            Some(ctx) => CtrlHeader::with_ctx(type_, ctx),
            None => CtrlHeader::new(type_),
        };
        self.request_nodata(
            CmdResource {
                hdr,
                resource_id: resource.to_wire(),
                padding: 0,
            },
            &[],
        )
    }

    fn attach_backing(
        &mut self,
        resource: ResourceId,
        addr: u64,
        length: u32,
    ) -> Result<(), GpuError> {
        let entry = MemEntry {
            addr,
            length,
            padding: 0,
        };
        self.request_nodata(
            CmdResourceAttachBacking {
                hdr: CtrlHeader::new(CMD_RESOURCE_ATTACH_BACKING),
                resource_id: resource.to_wire(),
                nr_entries: 1,
            },
            entry.as_bytes(),
        )
    }

    fn set_scanout_resource(&mut self, resource: ResourceId) -> Result<(), GpuError> {
        self.request_nodata(
            CmdSetScanout {
                hdr: CtrlHeader::new(CMD_SET_SCANOUT),
                rect: self.scanout,
                scanout_id: SCANOUT_ID,
                resource_id: resource.to_wire(),
            },
            &[],
        )
    }

    fn resource_flush(&mut self, rect: Rect, resource: ResourceId) -> Result<(), GpuError> {
        self.request_nodata(
            CmdResourceFlush {
                hdr: CtrlHeader::new(CMD_RESOURCE_FLUSH),
                rect,
                resource_id: resource.to_wire(),
                padding: 0,
            },
            &[],
        )
    }

    /// Creates the 2D resource of `/dev/fb0`, attaches guest pages, and shows it.
    /// Returns the framebuffer pages.
    pub fn create_framebuffer(&mut self) -> Result<&DmaBuffer, GpuError> {
        let Rect { width, height, .. } = self.scanout;
        let len = width as usize * height as usize * 4;
        let buffer = DmaBuffer::new(len.div_ceil(PAGE_SIZE));
        self.request_nodata(
            CmdResourceCreate2d {
                hdr: CtrlHeader::new(CMD_RESOURCE_CREATE_2D),
                resource_id: ResourceId::FRAMEBUFFER.to_wire(),
                format: FORMAT_B8G8R8A8_UNORM,
                width,
                height,
            },
            &[],
        )?;
        self.attach_backing(ResourceId::FRAMEBUFFER, buffer.paddr(), len as u32)?;
        self.set_scanout_resource(ResourceId::FRAMEBUFFER)?;
        Ok(self.framebuffer.insert(buffer))
    }

    /// Copies the framebuffer pages to the host and shows them.
    pub fn flush_framebuffer(&mut self) -> Result<(), GpuError> {
        let rect = self.scanout;
        self.request_nodata(
            CmdTransferToHost2d {
                hdr: CtrlHeader::new(CMD_TRANSFER_TO_HOST_2D),
                rect,
                offset: 0,
                resource_id: ResourceId::FRAMEBUFFER.to_wire(),
                padding: 0,
            },
            &[],
        )?;
        self.resource_flush(rect, ResourceId::FRAMEBUFFER)
    }

    fn require_3d(&self) -> Result<(), GpuError> {
        if self.has_3d {
            Ok(())
        } else {
            Err(GpuError::No3d)
        }
    }

    pub fn create_context(&mut self) -> Result<ContextId, GpuError> {
        self.require_3d()?;
        let ctx = self.next_ctx;
        let mut debug_name = [0u8; 64];
        debug_name[..DEBUG_NAME.len()].copy_from_slice(DEBUG_NAME);
        self.request_nodata(
            CmdCtxCreate {
                hdr: CtrlHeader::with_ctx(CMD_CTX_CREATE, ctx),
                nlen: DEBUG_NAME.len() as u32,
                context_init: 0,
                debug_name,
            },
            &[],
        )?;
        self.next_ctx = ctx.next();
        Ok(ctx)
    }

    /// Releases every resource of `ctx` and destroys the context.
    pub fn destroy_context(&mut self, ctx: ContextId) -> Result<(), GpuError> {
        if self.display_owner == Some(ctx) {
            self.display_owner = None;
            // The shown resource must change before `release` unrefs it. The
            // context goes away in either case, so a device error is ignored.
            let _ = self.set_scanout_resource(ResourceId::FRAMEBUFFER);
        }
        let owned: Vec<ResourceId> = self
            .resources
            .iter()
            .filter(|(_, res)| res.importers.contains(&ctx))
            .map(|(id, _)| *id)
            .collect();
        for id in owned {
            self.release(ctx, id)?;
        }
        self.request_nodata(CtrlHeader::with_ctx(CMD_CTX_DESTROY, ctx), &[])
    }

    /// Imports a shared memory region as a resource of `ctx`.
    /// The same region imported twice yields the same resource id.
    pub fn import(
        &mut self,
        ctx: ContextId,
        region: MmapRegion,
        kind: GpuResourceKind,
        width: u32,
        height: u32,
    ) -> Result<ResourceId, GpuError> {
        let phys = AddressSpace::kernel()
            .translate(VirtAddr::from_ptr(region.ptr().as_ptr()))
            .ok_or(GpuError::InvalidArgument)?;
        if !phys.is_aligned(Size4KiB::SIZE) {
            return Err(GpuError::InvalidArgument);
        }
        let required = match kind {
            GpuResourceKind::Texture => (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or(GpuError::InvalidArgument)?,
            GpuResourceKind::Buffer => {
                if height != 1 {
                    return Err(GpuError::InvalidArgument);
                }
                width as usize
            }
        };
        if required == 0 || region.len() < required {
            return Err(GpuError::InvalidArgument);
        }
        let len = u32::try_from(region.len()).map_err(|_| GpuError::InvalidArgument)?;
        let key = phys.as_u64();

        if let Some(&id) = self.by_phys.get(&key) {
            let res = &self.resources[&id];
            if res.kind != kind || res.width != width || res.height != height {
                return Err(GpuError::InvalidArgument);
            }
            if !res.importers.contains(&ctx) {
                self.resource_command(CMD_CTX_ATTACH_RESOURCE, Some(ctx), id)?;
                self.resources.get_mut(&id).unwrap().importers.insert(ctx);
            }
            return Ok(id);
        }

        let id = self.next_resource;
        let (target, format, bind) = match kind {
            GpuResourceKind::Texture => (
                PIPE_TEXTURE_2D,
                FORMAT_B8G8R8A8_UNORM,
                VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW | VIRGL_BIND_SCANOUT,
            ),
            GpuResourceKind::Buffer => {
                (PIPE_BUFFER, VIRGL_FORMAT_R8_UNORM, VIRGL_BIND_VERTEX_BUFFER)
            }
        };
        self.request_nodata(
            CmdResourceCreate3d {
                hdr: CtrlHeader::with_ctx(CMD_RESOURCE_CREATE_3D, ctx),
                resource_id: id.to_wire(),
                target,
                format,
                bind,
                width,
                height,
                depth: 1,
                array_size: 1,
                last_level: 0,
                nr_samples: 0,
                flags: 0,
                padding: 0,
            },
            &[],
        )?;
        self.next_resource = id.next();
        self.attach_backing(id, key, len)?;
        self.resource_command(CMD_CTX_ATTACH_RESOURCE, Some(ctx), id)?;
        self.resources.insert(
            id,
            ImportedResource {
                _region: region,
                kind,
                width,
                height,
                phys: key,
                importers: BTreeSet::from([ctx]),
            },
        );
        self.by_phys.insert(key, id);
        Ok(id)
    }

    /// Detaches `resource` from `ctx`; destroys it when no context uses it.
    pub fn release(&mut self, ctx: ContextId, resource: ResourceId) -> Result<(), GpuError> {
        let res = self
            .resources
            .get_mut(&resource)
            .ok_or(GpuError::InvalidArgument)?;
        if !res.importers.remove(&ctx) {
            return Err(GpuError::InvalidArgument);
        }
        let last = res.importers.is_empty();
        self.resource_command(CMD_CTX_DETACH_RESOURCE, Some(ctx), resource)?;
        if last {
            self.resource_command(CMD_RESOURCE_DETACH_BACKING, None, resource)?;
            self.resource_command(CMD_RESOURCE_UNREF, None, resource)?;
            let res = self.resources.remove(&resource).unwrap();
            self.by_phys.remove(&res.phys);
        }
        Ok(())
    }

    /// Checks that `ctx` imported `resource` and that the rectangle lies inside it.
    fn checked_resource(
        &self,
        ctx: ContextId,
        resource: ResourceId,
        rect: Rect,
    ) -> Result<&ImportedResource, GpuError> {
        let res = self
            .resources
            .get(&resource)
            .ok_or(GpuError::InvalidArgument)?;
        if !res.importers.contains(&ctx) {
            return Err(GpuError::InvalidArgument);
        }
        let x_end = rect
            .x
            .checked_add(rect.width)
            .ok_or(GpuError::InvalidArgument)?;
        let y_end = rect
            .y
            .checked_add(rect.height)
            .ok_or(GpuError::InvalidArgument)?;
        if x_end > res.width || y_end > res.height {
            return Err(GpuError::InvalidArgument);
        }
        Ok(res)
    }

    pub fn transfer(&mut self, ctx: ContextId, args: GpuTransfer) -> Result<(), GpuError> {
        let rect = Rect {
            x: args.x,
            y: args.y,
            width: args.width,
            height: args.height,
        };
        let resource = ResourceId::from_abi(args.resource);
        let res = self.checked_resource(ctx, resource, rect)?;
        let (box_, offset, stride) = match res.kind {
            GpuResourceKind::Texture => (
                Box3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
                    w: rect.width,
                    h: rect.height,
                    d: 1,
                },
                (u64::from(rect.y) * u64::from(res.width) + u64::from(rect.x)) * 4,
                res.width * 4,
            ),
            GpuResourceKind::Buffer => (
                Box3d {
                    x: rect.x,
                    y: 0,
                    z: 0,
                    w: rect.width,
                    h: 1,
                    d: 1,
                },
                u64::from(rect.x),
                0,
            ),
        };
        let type_ = match args.direction {
            GpuTransferDirection::ToHost => CMD_TRANSFER_TO_HOST_3D,
            GpuTransferDirection::FromHost => CMD_TRANSFER_FROM_HOST_3D,
        };
        self.request_nodata(
            CmdTransferHost3d {
                hdr: CtrlHeader::with_ctx(type_, ctx),
                box_,
                offset,
                resource_id: resource.to_wire(),
                level: 0,
                stride,
                layer_stride: 0,
            },
            &[],
        )
    }

    /// Submits a virgl command stream and returns its fence id.
    pub fn submit(&mut self, ctx: ContextId, cmds: &[u8]) -> Result<u64, GpuError> {
        if cmds.is_empty() || !cmds.len().is_multiple_of(4) {
            return Err(GpuError::InvalidArgument);
        }
        let chunks = split_commands(cmds)?;
        let fence = self.last_submitted + 1;
        let last = chunks.len() - 1;
        let mut result = Ok(());
        for (i, chunk) in chunks.into_iter().enumerate() {
            let mut hdr = CtrlHeader::with_ctx(CMD_SUBMIT_3D, ctx);
            if i == last {
                hdr.flags = FLAG_FENCE;
                hdr.fence_id = fence;
            }
            result = self.request_nodata(
                CmdSubmit3d {
                    hdr,
                    size: chunk.len() as u32,
                    padding: 0,
                },
                chunk,
            );
            if result.is_err() {
                break;
            }
        }
        // The device answered every chunk it received, so the stream is finished
        // even when it reported an error.
        self.last_submitted = fence;
        self.last_completed = fence;
        result.map(|()| fence)
    }

    pub fn wait_fence(&self, fence: u64) -> Result<(), GpuError> {
        self.require_3d()?;
        if fence > self.last_submitted {
            return Err(GpuError::InvalidArgument);
        }
        Ok(())
    }

    pub fn set_scanout(&mut self, ctx: ContextId, resource: ResourceId) -> Result<(), GpuError> {
        if self.display_owner.is_some_and(|owner| owner != ctx) {
            return Err(GpuError::DisplayOwned);
        }
        let res = self
            .resources
            .get(&resource)
            .ok_or(GpuError::InvalidArgument)?;
        if res.kind != GpuResourceKind::Texture
            || !res.importers.contains(&ctx)
            || res.width != self.scanout.width
            || res.height != self.scanout.height
        {
            return Err(GpuError::InvalidArgument);
        }
        self.set_scanout_resource(resource)?;
        self.display_owner = Some(ctx);
        Ok(())
    }

    pub fn flush(&mut self, ctx: ContextId, args: GpuFlush) -> Result<(), GpuError> {
        if self.display_owner != Some(ctx) {
            return Err(GpuError::DisplayOwned);
        }
        let rect = Rect {
            x: args.x,
            y: args.y,
            width: args.width,
            height: args.height,
        };
        let resource = ResourceId::from_abi(args.resource);
        self.checked_resource(ctx, resource, rect)?;
        self.resource_flush(rect, resource)
    }
}

impl Drop for GpuDevice {
    fn drop(&mut self) {
        self.transport.queue_unset(CONTROL_QUEUE);
    }
}

fn check_response(hdr: &CtrlHeader, expected: u32) -> Result<(), GpuError> {
    if hdr.type_ == expected {
        Ok(())
    } else if (RESP_ERR_FIRST..=RESP_ERR_LAST).contains(&hdr.type_) {
        Err(GpuError::Device(hdr.type_))
    } else {
        Err(GpuError::Device(RESP_ERR_FIRST))
    }
}

/// Splits a virgl stream at command boundaries into chunks that fit one `SUBMIT_3D`.
/// A command that runs past the end of `cmds` is a truncated stream.
fn split_commands(cmds: &[u8]) -> Result<Vec<&[u8]>, GpuError> {
    let mut chunks = Vec::new();
    let mut chunk_start = 0;
    let mut pos = 0;
    while pos < cmds.len() {
        let header = u32::from_ne_bytes(cmds[pos..pos + 4].try_into().unwrap());
        let cmd_len = (1 + (header >> 16) as usize) * 4;
        if cmd_len > cmds.len() - pos || cmd_len > MAX_CHUNK_BYTES {
            return Err(GpuError::InvalidArgument);
        }
        if pos + cmd_len - chunk_start > MAX_CHUNK_BYTES {
            chunks.push(&cmds[chunk_start..pos]);
            chunk_start = pos;
        }
        pos += cmd_len;
    }
    chunks.push(&cmds[chunk_start..]);
    Ok(chunks)
}

const _: () =
    assert!(size_of::<CmdSubmit3d>() + MAX_CHUNK_BYTES <= SUBMIT_STAGING_PAGES * PAGE_SIZE);
const _: () = assert!(SUBMIT_STAGING_PAGES == 65);
const _: () = assert!(size_of::<RespDisplayInfo>() <= PAGE_SIZE);
