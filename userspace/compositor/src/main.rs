//! The compositor owns the display. It serves window clients on
//! `SOCKET_PATH`, reads the input devices and draws the cursor. The worker
//! threads send events through one channel. The socket thread accepts the
//! clients and reads their requests. Each input device has its own input
//! thread. The main thread owns the display and the windows. It draws and
//! writes the responses. The escape key powers the machine off.
//!
//! The scene texture holds the desktop without the cursor. Each change to
//! the screen copies the changed part of the scene to the scanout. Then it
//! draws the cursor on top. Thus a cursor movement never makes a client draw
//! again.
//!
//! With `--probe`, the compositor reads pixels back from the scanout and
//! prints them. The end-to-end test checks these lines. No other user needs
//! them.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::ffi::c_int;
use std::net::Shutdown;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::exit;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, SendError, Sender};
use std::time::Instant;
use std::{env, io, thread};

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
use kernel_abi::{InputEvent, InputEventKind, PollEvents, PollFd};
use muffin_sys::{EINTR, ENOENT, Errno, O_RDONLY};

const CURSOR_VERTEX_BYTES: usize = VERTEX_COUNT * GOURAUD_STRIDE;

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

/// The socket thread gives each client a new id. An id never repeats.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ClientId(u64);

/// The worker threads send these events to the main thread. The events of
/// one client arrive in the sequence that the socket thread sends them.
enum Event {
    /// The socket thread accepts a client. The requests of `client` follow.
    Connected {
        client: ClientId,
        stream: Arc<UnixStream>,
    },
    Request {
        client: ClientId,
        request: Request,
    },
    /// The client closes its stream or sends a bad message. The socket thread
    /// does not read it again.
    Closed(ClientId),
    /// This is one batch of events of one input device, in the sequence of the device.
    Input(Vec<InputEvent>),
    /// A worker thread fails with this error. The compositor stops too.
    Fatal(io::Error),
}

fn main_thread_ended(_: SendError<Event>) -> io::Error {
    io::Error::other("the main thread ended")
}

/// This function runs `work` on a new thread. The main thread receives the error of `work`
/// as `Event::Fatal`.
fn spawn_worker(
    events: Sender<Event>,
    work: impl FnOnce(&Sender<Event>) -> io::Result<Infallible> + Send + 'static,
) -> io::Result<()> {
    thread::Builder::new().spawn(move || {
        let Err(e) = work(&events);
        // The send fails only when the main thread no longer runs. Then the
        // process exits, and no thread can report the error.
        let _ = events.send(Event::Fatal(e));
    })?;
    Ok(())
}

/// This is the write side of one client. Only the main thread writes to a
/// client.
struct Client {
    stream: Arc<UnixStream>,
    window: Option<Window>,
}

impl Client {
    /// Answers one request.
    fn serve(&mut self, request: Request, frame: &mut Frame) -> io::Result<()> {
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
        write_message(&mut &*self.stream, &response)?;
        if let Some(fd) = pending_fd {
            muffin_sys::send_fd(self.stream.as_raw_fd(), fd)
                .map_err(|e| errno_error("send_fd", e))?;
        }
        Ok(())
    }
}

/// This is one node under `/dev/input`. Each node has its own input thread.
struct InputDevice {
    path: String,
    fd: c_int,
    buf: [u8; InputEvent::SIZE * 64],
}

impl InputDevice {
    /// Opens every input device. devfs cannot list a directory. Thus this
    /// function tries the nodes in sequence until one is missing.
    fn open_all() -> io::Result<Vec<Self>> {
        let mut devices = Vec::new();
        for index in 0.. {
            let path = format!("/dev/input/event{index}");
            let fd = match muffin_sys::open(&path, O_RDONLY) {
                Ok(fd) => fd,
                Err(ENOENT) => break,
                Err(e) => return Err(errno_error(&path, e)),
            };
            println!("compositor: input {path}");
            devices.push(Self {
                path,
                fd,
                buf: [0; InputEvent::SIZE * 64],
            });
        }
        Ok(devices)
    }

    /// Reads the pending events. Blocks when no event is pending.
    fn read_events(&mut self) -> io::Result<Vec<InputEvent>> {
        let n = loop {
            match muffin_sys::read(self.fd, &mut self.buf) {
                Ok(n) => break n,
                Err(EINTR) => {}
                Err(e) => return Err(errno_error(&self.path, e)),
            }
        };
        Ok(self.buf[..n]
            .as_chunks::<{ InputEvent::SIZE }>()
            .0
            .iter()
            .map(|raw| InputEvent::from_bytes(*raw))
            .collect())
    }

    /// This function runs on the input thread of the device. It sends each batch of events to the main
    /// thread.
    fn forward_events(mut self, events: &Sender<Event>) -> io::Result<Infallible> {
        loop {
            let batch = self.read_events()?;
            events
                .send(Event::Input(batch))
                .map_err(main_thread_ended)?;
        }
    }
}

impl Drop for InputDevice {
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

/// This is the read side of one client. Only the socket thread reads from a
/// client.
struct ClientReader {
    id: ClientId,
    stream: Arc<UnixStream>,
}

impl ClientReader {
    fn poll_fd(&self) -> PollFd {
        PollFd::new(self.stream.as_raw_fd(), PollEvents::IN)
    }

    /// This function reads one full request. It returns `None` when the client closes the
    /// stream or sends a bad message.
    fn read_request(&self) -> Option<Request> {
        read_message::<Request>(&mut &*self.stream).unwrap_or_else(|e| {
            println!("compositor: client error {e}");
            None
        })
    }
}

/// The socket thread accepts the clients and reads their requests.
struct SocketReader {
    listener: UnixListener,
    clients: Vec<ClientReader>,
    next_client_id: u64,
}

impl SocketReader {
    fn new(listener: UnixListener) -> Self {
        Self {
            listener,
            clients: Vec::new(),
            next_client_id: 0,
        }
    }

    /// This function runs on the socket thread. It sends each new client and each request to
    /// the main thread.
    fn forward_requests(mut self, events: &Sender<Event>) -> io::Result<Infallible> {
        let mut fds = Vec::new();
        loop {
            fds.clear();
            fds.push(PollFd::new(self.listener.as_raw_fd(), PollEvents::IN));
            fds.extend(self.clients.iter().map(ClientReader::poll_fd));
            match muffin_sys::poll(&mut fds, None) {
                Ok(_) => {}
                Err(EINTR) => continue,
                Err(e) => return Err(errno_error("poll", e)),
            }
            let (listener_ready, client_ready) = (fds[0], &fds[1..]);

            if !listener_ready.revents.is_empty()
                && let Some(event) = self.accept()
            {
                events.send(event).map_err(main_thread_ended)?;
            }
            // Read from the back so a removal does not shift the entries ahead.
            for (index, ready) in client_ready.iter().enumerate().rev() {
                if ready.revents.is_empty() {
                    continue;
                }
                let client = &self.clients[index];
                let event = match client.read_request() {
                    Some(request) => Event::Request {
                        client: client.id,
                        request,
                    },
                    None => Event::Closed(self.clients.swap_remove(index).id),
                };
                events.send(event).map_err(main_thread_ended)?;
            }
        }
    }

    /// This function returns `None` when the accept fails. A failed accept does not stop
    /// the compositor.
    fn accept(&mut self) -> Option<Event> {
        let stream = match self.listener.accept() {
            Ok((stream, _)) => Arc::new(stream),
            Err(e) => {
                println!("compositor: accept error {e}");
                return None;
            }
        };
        let client = ClientId(self.next_client_id);
        self.next_client_id += 1;
        self.clients.push(ClientReader {
            id: client,
            stream: Arc::clone(&stream),
        });
        Some(Event::Connected { client, stream })
    }
}

/// The main thread owns the display, the windows and the write side of each
/// client.
struct Compositor {
    frame: Frame,
    clients: BTreeMap<ClientId, Client>,
    reporter: Option<CursorReporter>,
}

impl Compositor {
    fn handle(&mut self, event: Event) -> io::Result<()> {
        match event {
            Event::Connected { client, stream } => {
                self.clients.insert(
                    client,
                    Client {
                        stream,
                        window: None,
                    },
                );
            }
            Event::Request { client, request } => self.serve(client, request),
            Event::Closed(client) => {
                self.clients.remove(&client);
            }
            Event::Input(batch) => {
                const KEY_ESC: u16 = 1;
                if batch
                    .iter()
                    .any(|e| e.matches(InputEventKind::Key, KEY_ESC) && e.is_pressed())
                {
                    muffin_sys::poweroff();
                }
                let changes = self.frame.move_cursor(batch)?;
                if let Some(reporter) = self.reporter.as_mut() {
                    reporter.report(&self.frame, &changes)?;
                }
            }
            Event::Fatal(e) => return Err(e),
        }
        Ok(())
    }

    fn serve(&mut self, id: ClientId, request: Request) {
        // A client that fails has no entry. Its remaining requests get no
        // answer.
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        if let Err(e) = client.serve(request, &mut self.frame) {
            println!("compositor: client error {e}");
            // The shutdown wakes the socket thread. It then reads the end of
            // the stream and drops its read side.
            if let Err(e) = client.stream.shutdown(Shutdown::Both) {
                println!("compositor: shutdown error {e}");
            }
            self.clients.remove(&id);
        }
    }
}

fn run() -> io::Result<()> {
    let is_probing = env::args().skip(1).any(|arg| arg == "--probe");
    let frame = Frame::new(is_probing)?;
    let devices = InputDevice::open_all()?;
    let listener = UnixListener::bind(SOCKET_PATH)?;
    println!("compositor: listening on {SOCKET_PATH}");

    let (events, received) = mpsc::channel();
    for device in devices {
        spawn_worker(events.clone(), move |events| device.forward_events(events))?;
    }
    spawn_worker(events, move |events| {
        SocketReader::new(listener).forward_requests(events)
    })?;

    let mut compositor = Compositor {
        frame,
        clients: BTreeMap::new(),
        reporter: is_probing.then_some(CursorReporter {
            has_reported: false,
        }),
    };
    for event in received {
        compositor.handle(event)?;
    }
    Err(io::Error::other("every worker thread ended"))
}

fn main() {
    if let Err(e) = run() {
        println!("compositor: FAIL {e}");
        exit(1);
    }
}
