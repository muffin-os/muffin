//! The compositor owns the display. It serves window clients on
//! `SOCKET_PATH`, reads the mouse and draws the cursor. The process has one
//! thread, so one `poll` loop drives the listener, each client and the mouse.
//!
//! The scene texture holds the desktop without the cursor. Each change to
//! the screen copies the changed part of the scene to the scanout. Then it
//! draws the cursor on top. Thus a cursor movement never makes a client draw
//! again.
//!
//! With `--probe`, the compositor reads pixels back from the scanout and
//! prints them. The end-to-end test checks these lines. No other user needs
//! them.

use std::ffi::c_int;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::exit;
use std::rc::Rc;
use std::time::Instant;
use std::{env, io};

use compositor_cursor::arrow::{Arrow, Fill, Outline, VERTEX_COUNT};
use compositor_cursor::pointer::{Pointer, PointerChange};
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
use kernel_abi::{InputEvent, InputEventKind, MotionAxis, PollEvents, PollFd};
use muffin_sys::{EINTR, ENOENT, Errno, O_RDONLY};

const CURSOR_VERTEX_BYTES: usize = VERTEX_COUNT * GOURAUD_STRIDE;
/// This poll entry is for a missing mouse.
const NO_MOUSE: PollFd = PollFd::new(-1, PollEvents::IN);

// Both probes assume the 800x600 test resolution and the apex at `y = 0.75`.
/// A pixel inside the triangle, near the red apex.
const PROBE_IN_TRIANGLE: (u32, u32) = (400, 100);
/// A pixel below the triangle, where only the clear colour shows.
const PROBE_BELOW_TRIANGLE: (u32, u32) = (400, 560);

fn gfx_error(e: GfxError) -> io::Error {
    io::Error::other(format!("gfx {e:?}"))
}

fn errno_error(what: &str, e: Errno) -> io::Error {
    io::Error::other(format!("{what} {e:?}"))
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
    /// This texture holds the desktop without the cursor.
    scene: VirtioTexture,
    scene_queue: VirtioQueue,
    scanout: VirtioTexture,
    scanout_queue: VirtioQueue,
    pipeline: VirtioPipeline,
    triangle_vertices: VirtioBuffer,
    cursor_vertices: VirtioBuffer,
    pointer: Pointer,
    arrow: Arrow,
    width: u32,
    height: u32,
    /// When this is true, the compositor reads pixels back and prints them for
    /// the end-to-end test.
    is_probing: bool,
}

impl Frame {
    fn new(is_probing: bool) -> io::Result<Self> {
        let device = VirtioDevice::open_display().map_err(gfx_error)?;
        let info = device.info();
        let (width, height) = (info.width, info.height);
        let mut backend = VirtioBackend::new(device.clone());
        let desc = TextureDesc {
            width,
            height,
            format: TextureFormat::Rgba8Unorm,
        };
        let scanout = backend.alloc_texture(&desc).map_err(gfx_error)?;
        let scene = backend.alloc_texture(&desc).map_err(gfx_error)?;
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
        let mut triangle_vertices = backend
            .alloc_buffer(&BufferDesc {
                size: bytes.len(),
                is_dynamic: false,
            })
            .map_err(gfx_error)?;
        triangle_vertices.data_mut().copy_from_slice(&bytes);
        let cursor_vertices = backend
            .alloc_buffer(&BufferDesc {
                size: CURSOR_VERTEX_BYTES,
                is_dynamic: true,
            })
            .map_err(gfx_error)?;

        let scene_queue = VirtioQueue::new(device.clone(), &scene).map_err(gfx_error)?;
        let scanout_queue = VirtioQueue::new(device.clone(), &scanout).map_err(gfx_error)?;
        Ok(Self {
            device,
            backend,
            scene,
            scene_queue,
            scanout,
            scanout_queue,
            pipeline,
            triangle_vertices,
            cursor_vertices,
            pointer: Pointer::new(width, height),
            arrow: Arrow::new(width, height),
            width,
            height,
            is_probing,
        })
    }

    fn full_rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }

    /// Draws the triangle into the scene and shows it.
    fn draw(&mut self) -> io::Result<()> {
        let _fence = self
            .scene_queue
            .submit(|rec| {
                rec.clear(0);
                rec.bind_pipeline(&self.pipeline);
                rec.bind_vertex_buffer(&self.triangle_vertices);
                rec.draw(3)
            })
            .map_err(gfx_error)?;
        self.show(self.full_rect())
    }

    fn cursor_bounds(&self) -> Rect {
        self.arrow.bounds(self.pointer.position())
    }

    /// Copies `rect` of the scene to the scanout, draws the cursor on top
    /// when `rect` touches it, and shows the result.
    fn show(&mut self, rect: Rect) -> io::Result<()> {
        let touches_cursor = rect.intersects(self.cursor_bounds());
        if touches_cursor {
            self.pack_cursor_vertices();
        }
        let _fence = self
            .scanout_queue
            .submit(|rec| {
                rec.blit(&self.scene, rect, rect.x, rect.y)?;
                if touches_cursor {
                    rec.bind_pipeline(&self.pipeline);
                    rec.bind_vertex_buffer(&self.cursor_vertices);
                    rec.draw(VERTEX_COUNT as u32)?;
                }
                Ok(())
            })
            .map_err(gfx_error)?;
        self.scanout_queue.present().map_err(gfx_error)
    }

    /// Writes the cursor as gouraud vertices. Each vertex is `(x, y, r, g, b)` as `f32`.
    fn pack_cursor_vertices(&mut self) {
        let vertices = self
            .arrow
            .vertices(self.pointer.position(), self.pointer.buttons());
        let mut floats = self
            .cursor_vertices
            .data_mut()
            .as_chunks_mut::<{ size_of::<f32>() }>()
            .0
            .iter_mut();
        for vertex in vertices {
            for value in vertex.clip.into_iter().chain(vertex.rgb.0) {
                floats
                    .next()
                    .expect("the buffer holds CURSOR_VERTEX_BYTES")
                    .copy_from_slice(&value.to_ne_bytes());
            }
        }
    }

    /// Applies one batch of mouse events. When the batch moves the cursor or
    /// changes a button, it draws the old and the new area of the cursor again.
    /// Returns the changes of the batch in sequence.
    fn move_cursor(
        &mut self,
        events: impl IntoIterator<Item = InputEvent>,
    ) -> io::Result<Vec<PointerChange>> {
        let before = self.cursor_bounds();
        let changes: Vec<PointerChange> = events
            .into_iter()
            .filter_map(|event| self.pointer.apply(event))
            .collect();
        if !changes.is_empty() {
            self.show(before.union(self.cursor_bounds()))?;
        }
        Ok(changes)
    }

    /// Prints the cursor state and whether the scanout shows its paints.
    fn print_cursor(&self) -> io::Result<()> {
        let hotspot = self.pointer.position();
        let buttons = self.pointer.buttons();
        let (fill, outline) = (Fill::of(buttons), Outline::of(buttons));
        let pixels = match (
            self.try_probe(self.arrow.fill_probe(hotspot))?,
            self.try_probe(self.arrow.outline_probe(hotspot))?,
        ) {
            (Some(fill_pixel), Some(outline_pixel))
                if fill.rgb().matches(fill_pixel) && outline.rgb().matches(outline_pixel) =>
            {
                "ok".to_owned()
            }
            (Some(fill_pixel), Some(outline_pixel)) => {
                format!("0x{fill_pixel:08x},0x{outline_pixel:08x}")
            }
            _ => "offscreen".to_owned(),
        };
        let (x, y) = hotspot;
        println!(
            "compositor: cursor ({x},{y}) {buttons} fill={} outline={} pixels={pixels}",
            fill.name(),
            outline.name(),
        );
        Ok(())
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

    /// Returns `None` for a pixel outside the scanout.
    fn try_probe(&self, (x, y): (u32, u32)) -> io::Result<Option<u32>> {
        if x >= self.width || y >= self.height {
            return Ok(None);
        }
        self.probe((x, y)).map(Some)
    }

    /// Copies `rect` of the window to the same place in the scene and shows it.
    fn blit_region(&mut self, window: &Window, rect: Rect) -> io::Result<()> {
        let _fence = self
            .scene_queue
            .submit(|rec| rec.blit(&window.texture, rect, rect.x, rect.y))
            .map_err(gfx_error)?;
        self.show(rect)
    }
}

struct Window {
    texture: VirtioTexture,
    damage: Option<Rect>,
    has_blitted: bool,
}

impl Window {
    fn new(texture: VirtioTexture) -> Self {
        Self {
            texture,
            damage: None,
            has_blitted: false,
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

struct Client {
    stream: UnixStream,
    window: Option<Window>,
}

/// This is the result of one turn of a client.
enum Served {
    Request,
    /// The client closed the stream.
    Closed,
}

impl Client {
    fn poll_fd(&self) -> PollFd {
        PollFd::new(self.stream.as_raw_fd(), PollEvents::IN)
    }

    /// Answers one request.
    fn serve_one(&mut self, frame: &mut Frame) -> io::Result<Served> {
        let Some(request) = read_message::<Request>(&mut &self.stream)? else {
            return Ok(Served::Closed);
        };
        let mut pending_fd = None;
        let response = match request {
            Request::NewWindow(NewWindowRequest { width, height }) => {
                let start = Instant::now();
                frame.draw()?;
                println!(
                    "compositor: frame drawn {}x{} in {}us",
                    frame.width,
                    frame.height,
                    start.elapsed().as_micros()
                );
                if frame.is_probing {
                    frame.print_probes()?;
                }
                let texture = frame
                    .backend
                    .alloc_texture(&TextureDesc {
                        width,
                        height,
                        format: TextureFormat::Rgba8Unorm,
                    })
                    .map_err(gfx_error)?;
                println!("compositor: new window {width}x{height}");
                let created = Window::new(texture);
                pending_fd = Some(created.texture.fd());
                self.window = Some(created);
                Response::NewWindow(NewWindowResponse {})
            }
            Request::Damage(request) => {
                self.window.as_mut().ok_or_else(no_window)?.damage(&request);
                Response::Damage(DamageResponse {})
            }
            Request::Finish(FinishRequest { fence }) => {
                let window = self.window.as_mut().ok_or_else(no_window)?;
                if fence != 0 {
                    frame
                        .device
                        .wait_fence(fence)
                        .map_err(|_| io::Error::other("fence"))?;
                }
                if let Some(rect) = window.damage.take() {
                    frame.blit_region(window, rect)?;
                    if frame.is_probing && !window.has_blitted {
                        window.has_blitted = true;
                        let first = frame.probe((rect.x, rect.y))?;
                        println!(
                            "compositor: blit {}x{} first=0x{first:08x}",
                            rect.width, rect.height
                        );
                    }
                }
                Response::Finish(FinishResponse {})
            }
        };
        write_message(&mut &self.stream, &response)?;
        if let Some(fd) = pending_fd.take() {
            muffin_sys::send_fd(self.stream.as_raw_fd(), fd)
                .map_err(|e| errno_error("send_fd", e))?;
        }
        Ok(Served::Request)
    }
}

/// This is the first input device that reports motion on the X axis. One
/// mouse moves the cursor.
struct Mouse {
    fd: c_int,
    buf: [u8; InputEvent::SIZE * 64],
}

impl Mouse {
    /// Returns `None` when no input device moves on the X axis. devfs cannot
    /// list a directory. Thus this function tries the nodes in sequence until
    /// one is missing.
    fn open() -> io::Result<Option<Self>> {
        for index in 0.. {
            let path = format!("/dev/input/event{index}");
            let fd = match muffin_sys::open(&path, O_RDONLY) {
                Ok(fd) => fd,
                Err(ENOENT) => break,
                Err(e) => return Err(errno_error(&path, e)),
            };
            let has_motion_x = muffin_sys::input_supported_codes(fd, InputEventKind::Motion)
                .map_err(|e| errno_error(&path, e))?
                .contains(MotionAxis::X);
            if has_motion_x {
                println!("compositor: mouse {path}");
                return Ok(Some(Self {
                    fd,
                    buf: [0; InputEvent::SIZE * 64],
                }));
            }
            muffin_sys::close(fd).map_err(|e| errno_error(&path, e))?;
        }
        println!("compositor: no mouse");
        Ok(None)
    }

    fn poll_fd(&self) -> PollFd {
        PollFd::new(self.fd, PollEvents::IN)
    }

    /// Reads the pending events. Blocks when no event is pending.
    fn read_events(&mut self) -> io::Result<impl Iterator<Item = InputEvent> + '_> {
        let n = muffin_sys::read(self.fd, &mut self.buf).map_err(|e| errno_error("mouse", e))?;
        Ok(self.buf[..n]
            .as_chunks::<{ InputEvent::SIZE }>()
            .0
            .iter()
            .map(|raw| InputEvent::from_bytes(*raw)))
    }
}

impl Drop for Mouse {
    fn drop(&mut self) {
        let _ = muffin_sys::close(self.fd);
    }
}

/// Prints the cursor after its first change and after each button change.
/// Only `--probe` uses it.
struct CursorReporter {
    has_reported: bool,
}

impl CursorReporter {
    fn report(&mut self, frame: &Frame, changes: &[PointerChange]) -> io::Result<()> {
        let has_button_change = changes.iter().any(|change| change.is_button());
        if !changes.is_empty() && (!self.has_reported || has_button_change) {
            self.has_reported = true;
            frame.print_cursor()?;
        }
        Ok(())
    }
}

fn run() -> io::Result<()> {
    let is_probing = env::args().skip(1).any(|arg| arg == "--probe");
    let mut frame = Frame::new(is_probing)?;
    let mut mouse = Mouse::open()?;
    let mut reporter = is_probing.then_some(CursorReporter {
        has_reported: false,
    });
    let listener = UnixListener::bind(SOCKET_PATH)?;
    println!("compositor: listening on {SOCKET_PATH}");
    let mut clients: Vec<Client> = Vec::new();
    let mut fds = Vec::new();
    loop {
        fds.clear();
        fds.push(PollFd::new(listener.as_raw_fd(), PollEvents::IN));
        // Poll skips a negative descriptor. Thus a missing mouse needs no special case.
        fds.push(mouse.as_ref().map_or(NO_MOUSE, Mouse::poll_fd));
        fds.extend(clients.iter().map(Client::poll_fd));
        match muffin_sys::poll(&mut fds, None) {
            Ok(_) => {}
            Err(EINTR) => continue,
            Err(e) => return Err(errno_error("poll", e)),
        }
        let (listener_ready, mouse_ready, client_ready) = (fds[0], fds[1], &fds[2..]);

        if !listener_ready.revents.is_empty() {
            match listener.accept() {
                Err(e) => println!("compositor: accept error {e}"),
                Ok((stream, _)) => clients.push(Client {
                    stream,
                    window: None,
                }),
            }
        }
        if !mouse_ready.revents.is_empty()
            && let Some(mouse) = mouse.as_mut()
        {
            let changes = frame.move_cursor(mouse.read_events()?)?;
            if let Some(reporter) = reporter.as_mut() {
                reporter.report(&frame, &changes)?;
            }
        }
        // Serve from the back so a removal does not shift the entries ahead.
        for (index, ready) in client_ready.iter().enumerate().rev() {
            if ready.revents.is_empty() {
                continue;
            }
            match clients[index].serve_one(&mut frame) {
                Ok(Served::Request) => {}
                Ok(Served::Closed) => {
                    clients.swap_remove(index);
                }
                Err(e) => {
                    println!("compositor: client error {e}");
                    clients.swap_remove(index);
                }
            }
        }
    }
}

fn main() {
    if let Err(e) = run() {
        println!("compositor: FAIL {e}");
        exit(1);
    }
}
