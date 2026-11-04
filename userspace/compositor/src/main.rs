use std::fs::File;
use std::io::{self, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::exit;
use std::time::Instant;

use compositor_protocol::{
    NewWindowRequest, NewWindowResponse, Request, Response, SOCKET_PATH, read_message,
    write_message,
};
use gfx::api::{CommandRecorder, GfxAllocator, GfxCompiler, GfxQueue, PipelineDesc};
use gfx::backend::software::{
    ATTRIBUTE_BYTES, SoftAllocator, SoftBackend, SoftBuffer, SoftCompiler, SoftPipeline, SoftQueue,
    SoftShaderDef,
};
use gfx::error::GfxError;
use kernel_abi::gfx::BufferDesc;
use muffin_sys::{FbScreenInfo, IoctlRequest, MapFlags, ProtFlags, ioctl, mmap};

// Vertex layout (x, y, r, g, b).
fn color_vert(input: &[f32], output: &mut [f32]) {
    output[0] = input[0];
    output[1] = input[1];
    output[2] = input[2];
    output[3] = input[3];
    output[4] = input[4];
}

/// Scales a unit-interval channel to 0..=255, saturating outside it.
// Runs three times per covered pixel. In an unoptimised build only
// `inline(always)` keeps it from becoming an out-of-line call.
#[inline(always)]
fn channel_to_u8(value: f32) -> u32 {
    if value <= 0.0 {
        0
    } else if value >= 1.0 {
        255
    } else {
        (value * 255.0) as u32
    }
}

fn color_frag(interp: &[f32]) -> u32 {
    let r = channel_to_u8(interp[0]);
    let g = channel_to_u8(interp[1]);
    let b = channel_to_u8(interp[2]);
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

fn gfx_error(e: GfxError) -> io::Error {
    io::Error::other(format!("gfx {e:?}"))
}

struct Frame<'fb> {
    queue: SoftQueue<'fb>,
    pipeline: SoftPipeline,
    vertices: SoftBuffer,
    fb: File,
    width: u32,
    height: u32,
}

impl Frame<'_> {
    fn new() -> io::Result<Self> {
        let fb = File::open("/dev/fb0").map_err(|_| io::Error::other("open"))?;
        let fd = fb.as_raw_fd();

        let mut info = FbScreenInfo::default();
        ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info)
            .map_err(|_| io::Error::other("ioctl"))?;

        let len = info.len_bytes();
        let addr = mmap(
            0,
            len,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::SHARED,
            fd,
            0,
        )
        .map_err(|_| io::Error::other("mmap"))?;

        let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
        let vert = backend
            .compile_shader(&SoftShaderDef::Vertex {
                func: color_vert,
                output_count: 5,
            })
            .map_err(gfx_error)?;
        let frag = backend
            .compile_shader(&SoftShaderDef::Fragment(color_frag))
            .map_err(gfx_error)?;
        let pipeline = backend
            .compile_pipeline(&PipelineDesc {
                vertex_shader: &vert,
                pixel_shader: &frag,
                vertex_stride: 5 * ATTRIBUTE_BYTES,
            })
            .map_err(gfx_error)?;

        // three vertices: red apex top, green bottom-left, blue bottom-right
        #[rustfmt::skip]
        let verts: [f32; 15] = [
             0.00,  0.75, 1.0, 0.0, 0.0,
            -0.75, -0.75, 0.0, 1.0, 0.0,
             0.75, -0.75, 0.0, 0.0, 1.0,
        ];
        let bytes: Vec<u8> = verts.iter().flat_map(|f| f.to_ne_bytes()).collect();

        let mut vertices = backend
            .alloc_buffer(&BufferDesc {
                size: bytes.len(),
                is_dynamic: false,
            })
            .map_err(gfx_error)?;
        vertices.data.copy_from_slice(&bytes);

        // The software queue writes one u32 per pixel, so any other depth would
        // scribble over neighbouring pixels.
        let pixel_bytes = size_of::<u32>();
        let stride_px = info.pitch as usize / pixel_bytes;
        let width = info.width as usize;
        let height = info.height as usize;
        if info.bpp as usize != 8 * pixel_bytes
            || height == 0
            || width * pixel_bytes > info.pitch as usize
        {
            return Err(io::Error::other("geometry"));
        }
        // SAFETY: the mmap covers `len` bytes and is page-aligned, hence u32-aligned.
        // The geometry check above keeps `stride_px * (height - 1) + width` u32 words
        // within `len = pitch * height` bytes.
        let fb_u32 = unsafe {
            std::slice::from_raw_parts_mut(addr.cast::<u32>(), stride_px * (height - 1) + width)
        };
        // The mapped framebuffer's initial content is undefined and `SoftQueue::new`
        // never clears its target, so uncovered pixels would keep whatever the device
        // left there.
        fb_u32.fill(0);

        let queue = SoftQueue::new(fb_u32, info.width, info.height, stride_px);
        Ok(Self {
            queue,
            pipeline,
            vertices,
            fb,
            width: info.width,
            height: info.height,
        })
    }

    /// Draws the triangle and flushes the framebuffer.
    fn draw(&mut self) -> io::Result<()> {
        self.queue
            .submit(|rec| {
                rec.bind_pipeline(&self.pipeline);
                rec.bind_vertex_buffer(&self.vertices);
                rec.draw(3)
            })
            .map_err(gfx_error)?;
        self.fb.sync_all()
    }
}

fn serve(stream: &UnixStream, frame: &mut Frame<'_>) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut writer = stream;
    while let Some(request) = read_message::<Request>(&mut reader)? {
        match request {
            Request::NewWindow(NewWindowRequest { path }) => {
                println!("compositor: new window {}", path.display());
                let start = Instant::now();
                frame.draw()?;
                println!(
                    "compositor: frame drawn {}x{} in {}us",
                    frame.width,
                    frame.height,
                    start.elapsed().as_micros()
                );
                write_message(&mut writer, &Response::NewWindow(NewWindowResponse {}))?;
            }
        }
    }
    Ok(())
}

fn run() -> io::Result<()> {
    let mut frame = Frame::new()?;
    let listener = UnixListener::bind(SOCKET_PATH)?;
    println!("compositor: listening on {SOCKET_PATH}");
    for stream in listener.incoming() {
        match stream {
            Err(e) => println!("compositor: accept error {e}"),
            Ok(stream) => {
                if let Err(e) = serve(&stream, &mut frame) {
                    println!("compositor: client error {e}");
                }
            }
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        println!("compositor: FAIL {e}");
        exit(1);
    }
}
