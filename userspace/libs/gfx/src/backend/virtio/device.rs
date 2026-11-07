//! Everything that talks to `/dev/gpu0` or maps `/dev/shm` objects.

use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::Cell;
use core::ffi::c_int;
use core::ptr::NonNull;

use kernel_abi::gfx::{BufferDesc, TextureDesc};
use kernel_abi::{
    EEXIST, EINVAL, EIO, FdNum, GpuFenceInfo, GpuFlush, GpuImport, GpuInfo, GpuResourceKind,
    GpuSetScanout, GpuTransfer, GpuTransferDirection, GpuWaitFence, IoctlRequest, MapFlags,
    O_CREAT, O_EXCL, O_RDWR, ProtFlags, Stat,
};

use super::encode::Stream;
use super::{VIEWPORT_Y_SIGN, VirtioPipeline, VirtioShader, clip_blit};
use crate::api::{
    CommandRecorder, GfxAllocator, GfxBackend, GfxCompiler, GfxFence, GfxQueue, PipelineDesc, Rect,
};
use crate::error::{GfxError, Result};

const DISPLAY_PATH: &str = "/dev/gpu0";
const RENDER_PATH: &str = "/dev/render0";
const BYTES_PER_PIXEL: usize = 4;

/// One open descriptor of `/dev/gpu0` or `/dev/render0`. The kernel gives it its own virgl
/// context, so every object handle below is private to this device.
pub struct VirtioDevice {
    fd: c_int,
    info: GpuInfo,
    /// The next virgl object handle. Handles start at 1 because 0 unbinds.
    next_handle: Cell<u32>,
    /// The next suffix for a shared memory object of the allocator.
    next_shm: Cell<u32>,
}

fn ioctl<T>(fd: c_int, request: IoctlRequest, arg: &mut T) -> Result<()> {
    muffin_sys::ioctl(fd, request, arg)
        .map(|_| ())
        .map_err(GfxError::Device)
}

impl VirtioDevice {
    /// Opens `/dev/gpu0`. Fails with `DeviceUnavailable` when the file is
    /// missing or the device has no 3D support.
    pub fn open_display() -> Result<Rc<Self>> {
        Self::open_path(DISPLAY_PATH)
    }

    /// `set_scanout` and `flush` fail with `ENOTTY` on this device.
    pub fn open_render() -> Result<Rc<Self>> {
        Self::open_path(RENDER_PATH)
    }

    fn open_path(path: &str) -> Result<Rc<Self>> {
        let fd = muffin_sys::open(path, O_RDWR).map_err(|_| GfxError::DeviceUnavailable)?;
        // `device` owns `fd` from here on, so an early error closes it.
        let mut device = Self {
            fd,
            info: GpuInfo::default(),
            next_handle: Cell::new(1),
            next_shm: Cell::new(0),
        };
        ioctl(fd, IoctlRequest::GpuGetInfo, &mut device.info)?;
        if device.info.has_3d == 0 {
            return Err(GfxError::DeviceUnavailable);
        }
        Ok(Rc::new(device))
    }

    #[must_use]
    pub fn info(&self) -> GpuInfo {
        self.info
    }

    /// Returns a fresh virgl object handle.
    fn next_handle(&self) -> u32 {
        let handle = self.next_handle.get();
        self.next_handle.set(handle + 1);
        handle
    }

    /// Imports the file behind `fd` (a descriptor of this process) as a
    /// resource and returns the resource id.
    pub fn import(&self, fd: c_int, kind: GpuResourceKind, width: u32, height: u32) -> Result<u32> {
        let mut arg = GpuImport {
            fd: FdNum::from(fd),
            kind,
            width,
            height,
            resource: 0,
        };
        ioctl(self.fd, IoctlRequest::GpuImport, &mut arg)?;
        Ok(arg.resource)
    }

    /// Submits `stream` and returns the fence id of the submit.
    pub fn submit(&self, stream: &Stream) -> Result<u64> {
        let bytes = stream.as_bytes();
        let written = muffin_sys::write(self.fd, bytes).map_err(GfxError::Device)?;
        if written != bytes.len() {
            return Err(GfxError::Device(EIO));
        }
        Ok(self.fence_info()?.last_submitted)
    }

    pub fn transfer(&self, transfer: GpuTransfer) -> Result<()> {
        let mut arg = transfer;
        ioctl(self.fd, IoctlRequest::GpuTransfer, &mut arg)
    }

    pub fn fence_info(&self) -> Result<GpuFenceInfo> {
        let mut info = GpuFenceInfo::default();
        ioctl(self.fd, IoctlRequest::GpuFenceInfo, &mut info)?;
        Ok(info)
    }

    pub fn wait_fence(&self, fence: u64) -> Result<()> {
        let mut arg = GpuWaitFence { fence };
        ioctl(self.fd, IoctlRequest::GpuWaitFence, &mut arg)
    }

    /// Shows `tex` on the display. The texture must have the scanout size.
    pub fn set_scanout(&self, tex: &VirtioTexture) -> Result<()> {
        let mut arg = GpuSetScanout {
            resource: tex.resource(),
        };
        ioctl(self.fd, IoctlRequest::GpuSetScanout, &mut arg)
    }

    /// Updates `rect` of the display from `tex`.
    pub fn flush(&self, tex: &VirtioTexture, rect: Rect) -> Result<()> {
        self.flush_resource(tex.resource(), rect)
    }

    fn flush_resource(&self, resource: u32, rect: Rect) -> Result<()> {
        let mut arg = GpuFlush {
            resource,
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        };
        ioctl(self.fd, IoctlRequest::GpuFlush, &mut arg)
    }

    /// Copies `rect` of `tex` from the device into its memory. Afterwards
    /// `tex.pixels()` holds the rectangle.
    pub fn read_back(&self, tex: &VirtioTexture, rect: Rect) -> Result<()> {
        self.transfer(GpuTransfer {
            resource: tex.resource(),
            direction: GpuTransferDirection::FromHost,
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        })
    }

    /// Copies `rect` of `tex` from its memory to the device. Call it after a
    /// CPU write through `tex.pixels_mut()`; before it, the device does not
    /// see the write. The copy is complete when this function returns, so a
    /// later submit on any device reads the new pixels without a fence.
    pub fn upload(&self, tex: &VirtioTexture, rect: Rect) -> Result<()> {
        self.transfer(GpuTransfer {
            resource: tex.resource(),
            direction: GpuTransferDirection::ToHost,
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        })
    }

    /// Creates, maps and imports a private shared memory object. The name
    /// goes away right after the import; the descriptor and the mappings keep
    /// the object alive.
    fn alloc_resource(
        &self,
        len: usize,
        kind: GpuResourceKind,
        width: u32,
        height: u32,
    ) -> Result<(u32, Mapping)> {
        let pid = muffin_sys::getpid();
        let (file, path) = loop {
            let suffix = self.next_shm.get();
            self.next_shm.set(suffix + 1);
            let path = format!("/dev/shm/gfx-{pid}-{suffix}");
            match muffin_sys::open(&path, O_RDWR | O_CREAT | O_EXCL) {
                Ok(fd) => break (FileHandle(fd), path),
                Err(EEXIST) => continue,
                Err(errno) => return Err(GfxError::Device(errno)),
            }
        };
        let imported = muffin_sys::ftruncate(file.0, len as i64)
            .map_err(GfxError::Device)
            .and_then(|()| Mapping::new(file, len))
            .and_then(|mapping| {
                let resource = self.import(mapping.file.0, kind, width, height)?;
                Ok((resource, mapping))
            });
        let _ = muffin_sys::unlink(&path);
        imported
    }
}

impl Drop for VirtioDevice {
    fn drop(&mut self) {
        // The kernel releases the context and its resources with the descriptor.
        let _ = muffin_sys::close(self.fd);
    }
}

/// Closes the descriptor on drop.
struct FileHandle(c_int);

impl Drop for FileHandle {
    fn drop(&mut self) {
        let _ = muffin_sys::close(self.0);
    }
}

/// A shared memory object mapped into this process. The descriptor stays
/// open for the lifetime of the mapping.
struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
    file: FileHandle,
}

impl Mapping {
    /// Maps `len` bytes of `file` shared and writable.
    fn new(file: FileHandle, len: usize) -> Result<Self> {
        let mapped = muffin_sys::mmap(
            0,
            len,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::SHARED,
            file.0,
            0,
        );
        match mapped.map(NonNull::new) {
            Ok(Some(ptr)) => Ok(Self { ptr, len, file }),
            Ok(None) => Err(GfxError::Device(EINVAL)),
            Err(errno) => Err(GfxError::Device(errno)),
        }
    }

    fn bytes(&self) -> &[u8] {
        // Safety: the mapping stays valid for `len` bytes until `Drop`.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // Safety: the mapping stays valid for `len` bytes until `Drop`, and
        // `&mut self` is the only reference.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // Safety: no reference into the mapping outlives `self`. `file` drops
        // after this body runs, so the object is unmapped before it closes.
        let _ = unsafe { muffin_sys::munmap(self.ptr.as_ptr(), self.len) };
    }
}

/// A linear buffer resource that the CPU writes through [`VirtioBuffer::data_mut`].
pub struct VirtioBuffer {
    resource: u32,
    mapping: Mapping,
}

impl VirtioBuffer {
    #[must_use]
    pub fn data(&self) -> &[u8] {
        self.mapping.bytes()
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        self.mapping.bytes_mut()
    }

    #[must_use]
    pub fn resource(&self) -> u32 {
        self.resource
    }
}

/// A 2D texture resource with `0xAARRGGBB` pixels in its memory.
pub struct VirtioTexture {
    resource: u32,
    width: u32,
    height: u32,
    mapping: Mapping,
}

impl VirtioTexture {
    /// Returns the pixels in memory, row-major with `width` pixels per row.
    /// The memory shows the device image only after a read back.
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        let count = self.width as usize * self.height as usize;
        // Safety: the mapping is page-aligned and holds at least `count`
        // pixels, which the import checked.
        unsafe { core::slice::from_raw_parts(self.mapping.ptr.as_ptr().cast::<u32>(), count) }
    }

    /// Returns the pixels in memory for a CPU write, in the layout of
    /// [`VirtioTexture::pixels`]. The device sees a write only after
    /// [`VirtioDevice::upload`].
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        let count = self.width as usize * self.height as usize;
        // Safety: the mapping is page-aligned and holds at least `count`
        // pixels, which the import checked. `&mut self` is the only reference.
        unsafe { core::slice::from_raw_parts_mut(self.mapping.ptr.as_ptr().cast::<u32>(), count) }
    }

    #[must_use]
    pub fn resource(&self) -> u32 {
        self.resource
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Stays open for the life of the texture.
    #[must_use]
    pub fn fd(&self) -> c_int {
        self.mapping.file.0
    }
}

fn texture_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * BYTES_PER_PIXEL
}

/// Allocates resources and compiles pipelines on one [`VirtioDevice`].
pub struct VirtioBackend {
    device: Rc<VirtioDevice>,
}

impl VirtioBackend {
    #[must_use]
    pub fn new(device: Rc<VirtioDevice>) -> Self {
        Self { device }
    }

    /// Takes ownership of `file`.
    pub fn import_texture_fd(
        &mut self,
        file: c_int,
        width: u32,
        height: u32,
    ) -> Result<VirtioTexture> {
        let file = FileHandle(file);
        let len = texture_len(width, height);
        let mut stat = Stat::default();
        muffin_sys::fstat(file.0, &mut stat).map_err(GfxError::Device)?;
        if stat.size < len as u64 {
            return Err(GfxError::Device(EINVAL));
        }
        let mapping = Mapping::new(file, len)?;
        let resource =
            self.device
                .import(mapping.file.0, GpuResourceKind::Texture, width, height)?;
        Ok(VirtioTexture {
            resource,
            width,
            height,
            mapping,
        })
    }
}

impl GfxAllocator for VirtioBackend {
    type Buffer = VirtioBuffer;
    type Texture = VirtioTexture;

    fn alloc_buffer(&mut self, desc: &BufferDesc) -> Result<VirtioBuffer> {
        let (resource, mapping) =
            self.device
                .alloc_resource(desc.size, GpuResourceKind::Buffer, desc.size as u32, 1)?;
        Ok(VirtioBuffer { resource, mapping })
    }

    fn alloc_texture(&mut self, desc: &TextureDesc) -> Result<VirtioTexture> {
        let (resource, mapping) = self.device.alloc_resource(
            texture_len(desc.width, desc.height),
            GpuResourceKind::Texture,
            desc.width,
            desc.height,
        )?;
        Ok(VirtioTexture {
            resource,
            width: desc.width,
            height: desc.height,
            mapping,
        })
    }
}

impl GfxCompiler for VirtioBackend {
    type Pipeline = VirtioPipeline;
    type Shader = VirtioShader;
    type ShaderSource = str;

    fn compile_shader(&mut self, source: &str) -> Result<VirtioShader> {
        VirtioShader::parse(source)
    }

    fn compile_pipeline<'a>(
        &mut self,
        desc: &PipelineDesc<'a, VirtioShader>,
    ) -> Result<VirtioPipeline> {
        let handles = [(); 6].map(|()| self.device.next_handle());
        let mut stream = Stream::new();
        let pipeline = VirtioPipeline::encode(
            &mut stream,
            handles,
            desc.vertex_shader,
            desc.pixel_shader,
            desc.vertex_stride,
            desc.vertex_attributes,
        )?;
        self.device.submit(&stream)?;
        Ok(pipeline)
    }
}

impl GfxBackend for VirtioBackend {}

/// Renders into one texture through a virgl surface.
pub struct VirtioQueue {
    device: Rc<VirtioDevice>,
    target: u32,
    width: u32,
    height: u32,
    surface: u32,
}

impl VirtioQueue {
    /// Creates the surface of `target` on the device.
    pub fn new(device: Rc<VirtioDevice>, target: &VirtioTexture) -> Result<Self> {
        let surface = device.next_handle();
        let mut stream = Stream::new();
        stream.create_surface(surface, target.resource());
        device.submit(&stream)?;
        Ok(Self {
            device,
            target: target.resource(),
            width: target.width(),
            height: target.height(),
            surface,
        })
    }
}

impl GfxQueue<VirtioBackend> for VirtioQueue {
    type Recorder = VirtioRecorder;
    type Fence = VirtioFence;

    fn submit<F>(&mut self, recorder_cmds: F) -> Result<VirtioFence>
    where
        F: FnOnce(&mut Self::Recorder) -> Result<()>,
    {
        let mut rec = VirtioRecorder {
            stream: Stream::new(),
            pipeline: None,
            vertex_buffer: None,
            uploads: Vec::new(),
            target: self.target,
            width: self.width,
            height: self.height,
        };
        rec.stream.set_framebuffer_state(self.surface);
        rec.stream
            .set_viewport(self.width, self.height, VIEWPORT_Y_SIGN);
        recorder_cmds(&mut rec)?;
        // The CPU wrote the vertex buffers before this submit; the device
        // reads them only after a transfer.
        for (resource, len) in rec.uploads {
            self.device.transfer(GpuTransfer {
                resource,
                direction: GpuTransferDirection::ToHost,
                x: 0,
                y: 0,
                width: len as u32,
                height: 1,
            })?;
        }
        let id = self.device.submit(&rec.stream)?;
        Ok(VirtioFence {
            device: self.device.clone(),
            id,
        })
    }

    fn present(&mut self) -> Result<()> {
        let rect = Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        };
        self.device.flush_resource(self.target, rect)
    }
}

/// Records one virgl stream for a [`VirtioQueue`].
pub struct VirtioRecorder {
    stream: Stream,
    pipeline: Option<VirtioPipeline>,
    /// Resource id and byte length of the bound vertex buffer.
    vertex_buffer: Option<(u32, usize)>,
    /// Every vertex buffer that this recording binds. The submit transfers each one first.
    uploads: Vec<(u32, usize)>,
    target: u32,
    width: u32,
    height: u32,
}

impl CommandRecorder<VirtioBackend> for VirtioRecorder {
    fn bind_pipeline(&mut self, pso: &VirtioPipeline) {
        self.pipeline = Some(*pso);
        pso.bind(&mut self.stream);
    }

    fn bind_vertex_buffer(&mut self, buf: &VirtioBuffer) {
        let binding = (buf.resource(), buf.data().len());
        self.vertex_buffer = Some(binding);
        self.uploads.push(binding);
    }

    fn draw(&mut self, vertices: u32) -> Result<()> {
        let pipeline = self.pipeline.ok_or(GfxError::PipelineNotBound)?;
        let (resource, _) = self.vertex_buffer.ok_or(GfxError::VertexBufferNotBound)?;
        self.stream
            .set_vertex_buffers(pipeline.stride() as u32, resource);
        self.stream.draw_triangles(vertices);
        Ok(())
    }

    fn clear(&mut self, argb: u32) {
        self.stream.clear(argb);
    }

    fn blit(&mut self, src: &VirtioTexture, src_rect: Rect, dst_x: u32, dst_y: u32) -> Result<()> {
        let clipped = clip_blit(
            src.width(),
            src.height(),
            src_rect,
            dst_x,
            dst_y,
            self.width,
            self.height,
        );
        if let Some((src_rect, dst_rect)) = clipped {
            self.stream
                .blit(self.target, dst_rect, src.resource(), src_rect);
        }
        Ok(())
    }
}

/// The fence id of one submit on a [`VirtioDevice`].
pub struct VirtioFence {
    device: Rc<VirtioDevice>,
    id: u64,
}

impl GfxFence for VirtioFence {
    fn is_ready(&self) -> bool {
        self.device
            .fence_info()
            .is_ok_and(|info| info.last_completed >= self.id)
    }

    fn wait(&self) {
        let waited = self.device.wait_fence(self.id);
        debug_assert!(waited.is_ok(), "wait_fence({}) failed: {waited:?}", self.id);
    }

    fn id(&self) -> u64 {
        self.id
    }
}
