use std::f32::consts::TAU;
use std::ffi::c_int;
use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::exit;
use std::rc::Rc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use compositor_protocol::{
    DamageRequest, DamageResponse, FinishRequest, FinishResponse, NewWindowRequest,
    NewWindowResponse, Request, Response, SOCKET_PATH, read_message, write_message,
};
use gfx::api::{CommandRecorder, GfxAllocator, GfxCompiler, GfxFence, GfxQueue, PipelineDesc};
use gfx::backend::virtio::tgsi::{
    GOURAUD_ATTRIBUTES, GOURAUD_FRAGMENT, GOURAUD_STRIDE, GOURAUD_VERTEX,
};
use gfx::backend::virtio::{
    VirtioBackend, VirtioBuffer, VirtioDevice, VirtioPipeline, VirtioQueue, VirtioTexture,
};
use gfx::error::GfxError;
use kernel_abi::gfx::BufferDesc;

const CONNECT_ATTEMPTS: u32 = 50;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);
const WIDTH: u32 = 256;
const HEIGHT: u32 = 128;
const PIXEL: u32 = 0xFF20_60C0;

const TURN: Duration = Duration::from_secs(8);
const REPORT_PERIOD: Duration = Duration::from_secs(1);
/// After this many frame reports the demo goes silent and keeps drawing.
const MAX_REPORTS: u32 = 3;

/// Leaves a margin so the window background stays visible around the circle.
const CIRCLE_RADIUS_PX: u32 = 56;
const CIRCLE_SEGMENTS: u32 = 64;

/// The triangle the compositor draws; the circle shows its colour gradient.
const TRIANGLE: [[f32; 2]; 3] = [[0.0, 0.75], [-0.75, -0.75], [0.75, -0.75]];
/// Circumcircle of `TRIANGLE`, so each corner keeps its pure colour on the rim.
const CIRCUMCENTER_Y: f32 = -0.1875;
const CIRCUMRADIUS: f32 = 0.9375;

fn gfx_error(e: GfxError) -> io::Error {
    io::Error::other(format!("gfx {e:?}"))
}

// The barycentric weights double as the RGB channels; the fragment shader
// saturates weights outside the triangle.
fn triangle_color(x: f32, y: f32) -> [f32; 3] {
    let [[ax, ay], [bx, by], [cx, cy]] = TRIANGLE;
    let det = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy);
    let a = ((by - cy) * (x - cx) + (cx - bx) * (y - cy)) / det;
    let b = ((cy - ay) * (x - cx) + (ax - cx) * (y - cy)) / det;
    [a, b, 1.0 - a - b]
}

fn rim_vertex(segment: u32, phase: f32) -> [f32; 5] {
    let angle = segment as f32 * TAU / CIRCLE_SEGMENTS as f32;
    let (sin, cos) = angle.sin_cos();
    let (color_sin, color_cos) = (angle + phase).sin_cos();
    let [r, g, b] = triangle_color(
        CIRCUMRADIUS * color_cos,
        CIRCUMCENTER_Y + CIRCUMRADIUS * color_sin,
    );
    let radius = CIRCLE_RADIUS_PX as f32;
    [
        radius * cos / (WIDTH as f32 / 2.0),
        radius * sin / (HEIGHT as f32 / 2.0),
        r,
        g,
        b,
    ]
}

// Only the colours depend on `phase`; the geometry never changes, so no
// clear is needed between frames.
fn circle_vertices(phase: f32) -> Vec<u8> {
    let [r, g, b] = triangle_color(0.0, CIRCUMCENTER_Y);
    let center = [0.0, 0.0, r, g, b];
    (0..CIRCLE_SEGMENTS)
        .flat_map(|segment| {
            [
                center,
                rim_vertex(segment, phase),
                rim_vertex((segment + 1) % CIRCLE_SEGMENTS, phase),
            ]
        })
        .flatten()
        .flat_map(f32::to_ne_bytes)
        .collect()
}

struct Circle {
    pipeline: VirtioPipeline,
    vertices: VirtioBuffer,
    queue: VirtioQueue,
    // Keeps the window resource alive while the queue renders into it.
    _window: VirtioTexture,
}

impl Circle {
    fn new(device: Rc<VirtioDevice>, window_fd: c_int) -> io::Result<Self> {
        let mut backend = VirtioBackend::new(device.clone());
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
        let vertices = backend
            .alloc_buffer(&BufferDesc {
                size: CIRCLE_SEGMENTS as usize * 3 * GOURAUD_STRIDE,
                is_dynamic: true,
            })
            .map_err(gfx_error)?;
        let window = backend
            .import_texture_fd(window_fd, WIDTH, HEIGHT)
            .map_err(gfx_error)?;
        let queue = VirtioQueue::new(device, &window).map_err(gfx_error)?;
        Ok(Self {
            pipeline,
            vertices,
            queue,
            _window: window,
        })
    }

    fn damage() -> DamageRequest {
        DamageRequest {
            x: WIDTH / 2 - CIRCLE_RADIUS_PX,
            y: HEIGHT / 2 - CIRCLE_RADIUS_PX,
            width: 2 * CIRCLE_RADIUS_PX,
            height: 2 * CIRCLE_RADIUS_PX,
        }
    }

    /// Draws the circle and returns the fence id of the submit.
    fn draw(&mut self, phase: f32) -> io::Result<u64> {
        self.submit(phase, false)
    }

    /// Fills the window with `PIXEL` before the first circle.
    fn clear_and_draw(&mut self, phase: f32) -> io::Result<u64> {
        self.submit(phase, true)
    }

    fn submit(&mut self, phase: f32, clear: bool) -> io::Result<u64> {
        self.vertices
            .data_mut()
            .copy_from_slice(&circle_vertices(phase));
        let fence = self
            .queue
            .submit(|rec| {
                if clear {
                    rec.clear(PIXEL);
                }
                rec.bind_pipeline(&self.pipeline);
                rec.bind_vertex_buffer(&self.vertices);
                rec.draw(CIRCLE_SEGMENTS * 3)
            })
            .map_err(gfx_error)?;
        Ok(fence.id())
    }
}

fn phase(elapsed: Duration) -> f32 {
    (elapsed.as_millis() % TURN.as_millis()) as f32 * TAU / TURN.as_millis() as f32
}

/// Retries because `services` spawns the compositor first but does not wait for its socket.
fn connect() -> io::Result<UnixStream> {
    let mut attempt = 1;
    loop {
        match UnixStream::connect(SOCKET_PATH) {
            Ok(stream) => return Ok(stream),
            Err(e) if attempt == CONNECT_ATTEMPTS => return Err(e),
            Err(_) => {
                attempt += 1;
                sleep(CONNECT_RETRY_DELAY);
            }
        }
    }
}

fn call(stream: &UnixStream, request: &Request) -> io::Result<Response> {
    write_message(&mut &*stream, request)?;
    read_message::<Response>(&mut &*stream)?
        .ok_or_else(|| io::Error::other("compositor closed the connection"))
}

fn unexpected(response: Response) -> io::Error {
    io::Error::other(format!("unexpected response {response:?}"))
}

fn create_window(stream: &UnixStream, request: NewWindowRequest) -> io::Result<c_int> {
    match call(stream, &Request::NewWindow(request))? {
        Response::NewWindow(NewWindowResponse {}) => {}
        other => return Err(unexpected(other)),
    }
    muffin_sys::recv_fd(stream.as_raw_fd()).map_err(|e| io::Error::other(format!("recv_fd {e:?}")))
}

fn run() -> io::Result<()> {
    let stream = connect()?;
    println!("compositor-demo: connected");

    let window_fd = create_window(
        &stream,
        NewWindowRequest {
            width: WIDTH,
            height: HEIGHT,
        },
    )?;
    println!("compositor-demo: window created");

    let device = VirtioDevice::open_render().map_err(gfx_error)?;
    let mut circle = Circle::new(device, window_fd)?;
    let start = Instant::now();
    let fence = circle.clear_and_draw(phase(start.elapsed()))?;
    let full = DamageRequest {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    };
    match call(&stream, &Request::Damage(full))? {
        Response::Damage(DamageResponse {}) => {}
        other => return Err(unexpected(other)),
    }
    match call(&stream, &Request::Finish(FinishRequest { fence }))? {
        Response::Finish(FinishResponse {}) => {}
        other => return Err(unexpected(other)),
    }

    let mut report_start = Instant::now();
    let mut report_frames = 0u32;
    let mut reports_left = MAX_REPORTS;
    loop {
        let fence = circle.draw(phase(start.elapsed()))?;
        match call(&stream, &Request::Damage(Circle::damage()))? {
            Response::Damage(DamageResponse {}) => {}
            other => return Err(unexpected(other)),
        }
        match call(&stream, &Request::Finish(FinishRequest { fence }))? {
            Response::Finish(FinishResponse {}) => {}
            other => return Err(unexpected(other)),
        }
        report_frames += 1;
        if reports_left > 0 && report_start.elapsed() >= REPORT_PERIOD {
            println!(
                "compositor-demo: {report_frames} frames in the last {}ms",
                report_start.elapsed().as_millis()
            );
            report_start = Instant::now();
            report_frames = 0;
            reports_left -= 1;
        }
    }
}

fn main() {
    if let Err(e) = run() {
        println!("compositor-demo: FAIL {e}");
        exit(1);
    }
}
