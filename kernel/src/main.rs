#![no_std]
#![no_main]
extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::error::Error;
use core::slice;

use ext2::Ext2Fs;
use jiff::Timestamp;
use kernel::driver::KernelDeviceId;
use kernel::driver::block::BlockDevices;
use kernel::file::ext2::VirtualExt2Fs;
use kernel::file::vfs;
use kernel::limine::BASE_REVISION;
use kernel::mcore;
use kernel::mcore::mtask::process::Process;
use kernel::time::TimestampExt;
use kernel_abi::gfx::BufferDesc;
use kernel_device::block::{BlockBuf, BlockDevice};
use kernel_gfx::api::{CommandRecorder, GfxAllocator, GfxCompiler, GfxQueue, PipelineDesc};
use kernel_gfx::backend::software::{
    SoftAllocator, SoftBackend, SoftCompiler, SoftQueue, SoftShaderDef,
};
use kernel_vfs::Stat;
use kernel_vfs::path::{AbsolutePath, ROOT};
use log::info;
use spin::RwLock;

#[unsafe(export_name = "kernel_main")]
unsafe extern "C" fn main() -> ! {
    assert!(BASE_REVISION.is_supported());

    kernel::init();

    {
        info!("mounting root filesystem");
        let root_block_device = BlockDevices::by_id(0).expect("should have block device with id 0");
        let root_block_device = ArcLockedBlockDevice(root_block_device);
        vfs()
            .write()
            .mount(
                ROOT,
                VirtualExt2Fs::from(
                    Ext2Fs::try_new(root_block_device).expect("should be able to create ext2fs"),
                ),
            )
            .expect("should be able to mount ext2fs at /");
    }

    {
        info!("starting init process...");
        let init_path = AbsolutePath::try_new("/bin/init").unwrap();
        let _ = vfs().read().open(init_path).expect("should have /bin/init");
        let proc = Process::create_from_executable(Process::root(), init_path).unwrap();
        info!("started process pid={}", proc.pid());
    }

    render_demo_frame();

    mcore::turn_idle()
}

// ─── kernel_gfx demo: Gouraud-shaded triangle into /dev/fb0 ──────────────────

// Vertex layout: (x, y, r, g, b) — 5 floats × 4 bytes = 20 bytes/vertex.
fn color_vert(input: &[f32], output: &mut [f32]) {
    output[0] = input[0]; // NDC x
    output[1] = input[1]; // NDC y
    output[2] = input[2]; // red
    output[3] = input[3]; // green
    output[4] = input[4]; // blue
}

fn color_frag(interp: &[f32]) -> u32 {
    let r = (interp[0].clamp(0.0, 1.0) * 255.0) as u32;
    let g = (interp[1].clamp(0.0, 1.0) * 255.0) as u32;
    let b = (interp[2].clamp(0.0, 1.0) * 255.0) as u32;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

/// Renders a single Gouraud-shaded triangle into /dev/fb0 using the
/// `kernel_gfx` software rasterizer, then fsyncs to push the frame to the
/// display.
fn render_demo_frame() {
    const WIDTH: u32 = 710;
    const HEIGHT: u32 = 835;
    const BYTES: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

    info!("rendering kernel_gfx demo frame to /dev/fb0");

    let start = Timestamp::now();

    let fb_path = AbsolutePath::try_new("/dev/fb0").unwrap();
    let fb_node = vfs().read().open(fb_path).expect("should have /dev/fb0");

    let mut stat = Stat::default();
    fb_node.stat(&mut stat).expect("stat /dev/fb0");
    assert_eq!(
        stat.size, BYTES,
        "framebuffer size mismatch (expected {WIDTH}x{HEIGHT} BGRA)"
    );

    let mapped = fb_node.mmap().expect("mmap /dev/fb0");
    assert_eq!(mapped.len, BYTES);
    let fb_bytes: &mut [u8] = unsafe { slice::from_raw_parts_mut(mapped.ptr.as_ptr(), mapped.len) };
    let mmap_time = Timestamp::now();

    // Build the rasterizer pipeline.
    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let vert = backend
        .compile_shader(&SoftShaderDef::Vertex {
            func: color_vert,
            output_count: 5,
        })
        .unwrap();
    let frag = backend
        .compile_shader(&SoftShaderDef::Fragment(color_frag))
        .unwrap();
    let pso = backend
        .compile_pipeline(&PipelineDesc {
            vertex_shader: &vert,
            pixel_shader: &frag,
            blend: false,
            depth: false,
            vertex_stride: 20,
        })
        .unwrap();
    let compile_time = Timestamp::now();

    // Three vertices: red apex top, green bottom-left, blue bottom-right.
    #[rustfmt::skip]
    let verts: [f32; 15] = [
         0.00,  0.75, 1.0, 0.0, 0.0,
        -0.75, -0.75, 0.0, 1.0, 0.0,
         0.75, -0.75, 0.0, 0.0, 1.0,
    ];
    let bytes: Vec<u8> = verts.iter().flat_map(|f| f.to_ne_bytes()).collect();

    let mut vbuf = backend
        .alloc_buffer(&BufferDesc {
            size: bytes.len(),
            is_dynamic: false,
        })
        .unwrap();
    vbuf.data.copy_from_slice(&bytes);

    let alloc_time = Timestamp::now();

    let mut q = SoftQueue::new(WIDTH, HEIGHT);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&vbuf);
        rec.draw(3);
    })
    .unwrap();

    let record_time = Timestamp::now();

    // Convert SoftQueue's 0xAARRGGBB u32 pixels to virtio's [B, G, R, A] bytes.
    for (i, &p) in q.framebuffer().iter().enumerate() {
        let off = i * 4;
        fb_bytes[off] = (p) as u8; // B
        fb_bytes[off + 1] = (p >> 8) as u8; // G
        fb_bytes[off + 2] = (p >> 16) as u8; // R
        fb_bytes[off + 3] = (p >> 24) as u8; // A
    }

    let write_time = Timestamp::now();

    fb_node.fsync().expect("fsync /dev/fb0");

    let flush_time = Timestamp::now();
    info!(
        "kernel_gfx demo frame committed, mmap={:?}, compile={:?}, alloc={:?}, record={:?}, write={:?}, flush={:?}, total={:?}",
        mmap_time.since(start).unwrap(),
        compile_time.since(mmap_time).unwrap(),
        alloc_time.since(compile_time).unwrap(),
        record_time.since(alloc_time).unwrap(),
        write_time.since(record_time).unwrap(),
        flush_time.since(write_time).unwrap(),
        flush_time.since(start).unwrap(),
    );
}

struct ArcLockedBlockDevice<const N: usize>(
    Arc<RwLock<dyn BlockDevice<KernelDeviceId, N> + Send + Sync>>,
);

impl<const N: usize> filesystem::BlockDevice for ArcLockedBlockDevice<N> {
    type Error = Box<dyn Error>;

    fn sector_size(&self) -> usize {
        N
    }

    fn sector_count(&self) -> usize {
        self.0.read().block_count()
    }

    fn read_sector(&self, sector_index: usize, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut read_buf = BlockBuf::new();
        self.0.write().read_block(sector_index, &mut read_buf)?;
        buf.copy_from_slice(&read_buf[..]);
        Ok(buf.len())
    }

    fn write_sector(&mut self, sector_index: usize, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut write_buf = BlockBuf::new();
        write_buf.copy_from_slice(buf);
        self.0
            .write()
            .write_block(sector_index, &write_buf)
            .map(|()| buf.len())
    }
}

#[panic_handler]
#[cfg(not(test))]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    handle_panic(info);
    loop {
        x86_64::instructions::hlt();
    }
}

#[cfg(not(test))]
fn handle_panic(info: &core::panic::PanicInfo) {
    use log::error;

    let location = info.location().unwrap();
    error!(
        "kernel panicked at {}:{}:{}:",
        location.file(),
        location.line(),
        location.column(),
    );
    error!("{}", info.message());

    #[cfg(feature = "backtrace")]
    match kernel::backtrace::Backtrace::try_capture() {
        Ok(bt) => {
            error!("stack backtrace:\n{bt}");
        }
        Err(e) => {
            error!("error capturing backtrace: {e:?}");
        }
    }
}
