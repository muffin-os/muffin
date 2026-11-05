use std::io::{self, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::exit;
use std::rc::Rc;
use std::time::Instant;

use compositor_protocol::{
    DamageRequest, DamageResponse, FinishRequest, FinishResponse, NewWindowRequest,
    NewWindowResponse, Request, Response, SOCKET_PATH, read_message, write_message,
};
use gfx::api::{CommandRecorder, GfxAllocator, GfxCompiler, GfxQueue, PipelineDesc, Rect};
use gfx::backend::virtio::tgsi::{
    GOURAUD_ATTRIBUTES, GOURAUD_FRAGMENT, GOURAUD_STRIDE, GOURAUD_VERTEX,
};
use gfx::backend::virtio::{
    VirtioBackend, VirtioBuffer, VirtioDevice, VirtioPipeline, VirtioQueue, VirtioTexture,
};
use gfx::error::GfxError;
use kernel_abi::gfx::{BufferDesc, TextureDesc, TextureFormat};

// Both probes assume the 800x600 test resolution and the apex at `y = 0.75`.
/// A pixel inside the triangle, near the red apex.
const PROBE_IN_TRIANGLE: (u32, u32) = (400, 100);
/// A pixel below the triangle, where only the clear colour shows.
const PROBE_BELOW_TRIANGLE: (u32, u32) = (400, 560);

fn gfx_error(e: GfxError) -> io::Error {
    io::Error::other(format!("gfx {e:?}"))
}

/// Names the channel with the largest value of an `0xAARRGGBB` pixel.
fn dominant_channel(pixel: u32) -> &'static str {
    let r = (pixel >> 16) & 0xFF;
    let g = (pixel >> 8) & 0xFF;
    let b = pixel & 0xFF;
    if r == 0 && g == 0 && b == 0 {
        "none"
    } else if r >= g && r >= b {
        "red"
    } else if g >= b {
        "green"
    } else {
        "blue"
    }
}

struct Frame {
    device: Rc<VirtioDevice>,
    backend: VirtioBackend,
    queue: VirtioQueue,
    scanout: VirtioTexture,
    pipeline: VirtioPipeline,
    vertices: VirtioBuffer,
    width: u32,
    height: u32,
}

impl Frame {
    fn new() -> io::Result<Self> {
        let device = VirtioDevice::open().map_err(gfx_error)?;
        let info = device.info();
        let (width, height) = (info.width, info.height);
        let mut backend = VirtioBackend::new(device.clone());
        let scanout = backend
            .alloc_texture(&TextureDesc {
                width,
                height,
                format: TextureFormat::Rgba8Unorm,
            })
            .map_err(gfx_error)?;
        device.set_scanout(&scanout).map_err(gfx_error)?;
        println!("compositor: gpu scanout {width}x{height}");

        let vert = backend.compile_shader(GOURAUD_VERTEX).map_err(gfx_error)?;
        let frag = backend
            .compile_shader(GOURAUD_FRAGMENT)
            .map_err(gfx_error)?;
        let pipeline = backend
            .compile_pipeline(&PipelineDesc {
                vertex_shader: &vert,
                pixel_shader: &frag,
                vertex_stride: GOURAUD_STRIDE,
                vertex_attributes: &GOURAUD_ATTRIBUTES,
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
        vertices.data_mut().copy_from_slice(&bytes);

        let queue = VirtioQueue::new(device.clone(), &scanout).map_err(gfx_error)?;
        Ok(Self {
            device,
            backend,
            queue,
            scanout,
            pipeline,
            vertices,
            width,
            height,
        })
    }

    /// Draws the triangle and shows it.
    fn draw(&mut self) -> io::Result<()> {
        let _fence = self
            .queue
            .submit(|rec| {
                rec.clear(0);
                rec.bind_pipeline(&self.pipeline);
                rec.bind_vertex_buffer(&self.vertices);
                rec.draw(3)
            })
            .map_err(gfx_error)?;
        self.queue.present().map_err(gfx_error)
    }

    /// Prints two probe pixels of the scanout as evidence of the orientation.
    fn print_probes(&self) -> io::Result<()> {
        let in_triangle = self.probe(PROBE_IN_TRIANGLE)?;
        let below = self.probe(PROBE_BELOW_TRIANGLE)?;
        let (x, y) = PROBE_IN_TRIANGLE;
        println!(
            "compositor: probe ({x},{y}) dominant={}",
            dominant_channel(in_triangle)
        );
        let (x, y) = PROBE_BELOW_TRIANGLE;
        println!("compositor: probe ({x},{y})=0x{below:08x}");
        Ok(())
    }

    /// Reads one scanout pixel back from the device.
    fn probe(&self, (x, y): (u32, u32)) -> io::Result<u32> {
        let rect = Rect {
            x,
            y,
            width: 1,
            height: 1,
        };
        self.device
            .read_back(&self.scanout, rect)
            .map_err(gfx_error)?;
        Ok(self.scanout.pixels()[y as usize * self.width as usize + x as usize])
    }

    /// Copies `rect` of the window to the same place on the scanout and shows it.
    fn blit_region(&mut self, window: &Window, rect: Rect) -> io::Result<()> {
        let _fence = self
            .queue
            .submit(|rec| rec.blit(&window.texture, rect, rect.x, rect.y))
            .map_err(gfx_error)?;
        self.queue.present().map_err(gfx_error)
    }
}

struct Window {
    texture: VirtioTexture,
    damage: Option<Rect>,
}

impl Window {
    fn open(request: &NewWindowRequest, backend: &mut VirtioBackend) -> io::Result<Self> {
        let path = request
            .path
            .to_str()
            .ok_or_else(|| io::Error::other("path"))?;
        let texture = backend
            .import_texture(path, request.width, request.height)
            .map_err(gfx_error)?;
        Ok(Self {
            texture,
            damage: None,
        })
    }

    fn full_rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.texture.width(),
            height: self.texture.height(),
        }
    }

    fn damage(&mut self, request: &DamageRequest) {
        let width = self.texture.width();
        let height = self.texture.height();
        let x = request.x.min(width);
        let y = request.y.min(height);
        let width = request.x.saturating_add(request.width).min(width) - x;
        let height = request.y.saturating_add(request.height).min(height) - y;
        if width == 0 || height == 0 {
            return;
        }
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        self.damage = Some(self.damage.map_or(rect, |damage| damage.union(rect)));
    }
}

fn no_window() -> io::Error {
    io::Error::other("request before NewWindow")
}

fn serve(stream: &UnixStream, frame: &mut Frame) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut writer = stream;
    let mut window: Option<Window> = None;
    while let Some(request) = read_message::<Request>(&mut reader)? {
        let response = match request {
            Request::NewWindow(request) => {
                let NewWindowRequest {
                    path,
                    width,
                    height,
                } = &request;
                let start = Instant::now();
                frame.draw()?;
                println!(
                    "compositor: frame drawn {}x{} in {}us",
                    frame.width,
                    frame.height,
                    start.elapsed().as_micros()
                );
                frame.print_probes()?;
                println!("compositor: new window {} {width}x{height}", path.display());
                let opened = Window::open(&request, &mut frame.backend)?;
                let rect = opened.full_rect();
                frame.blit_region(&opened, rect)?;
                frame
                    .device
                    .read_back(&frame.scanout, rect)
                    .map_err(gfx_error)?;
                let first = frame.scanout.pixels()[0];
                println!("compositor: blit {width}x{height} first=0x{first:08x}");
                window = Some(opened);
                Response::NewWindow(NewWindowResponse {})
            }
            Request::Damage(request) => {
                window.as_mut().ok_or_else(no_window)?.damage(&request);
                Response::Damage(DamageResponse {})
            }
            Request::Finish(FinishRequest { fence }) => {
                let window = window.as_mut().ok_or_else(no_window)?;
                if fence != 0 {
                    frame
                        .device
                        .wait_fence(fence)
                        .map_err(|_| io::Error::other("fence"))?;
                }
                if let Some(rect) = window.damage.take() {
                    frame.blit_region(window, rect)?;
                }
                Response::Finish(FinishResponse {})
            }
        };
        write_message(&mut writer, &response)?;
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
