//! A text shell in a compositor window. The window covers 60% of the screen
//! in each direction, in the centre. The input thread fills an
//! `InputBuffer`. The main thread turns the key events into characters,
//! runs the commands and draws the text with the CPU.
//!
//! The commands are `echo`, `cd`, `ls`, `tree` and `poweroff`. The mouse
//! wheel scrolls through the earlier output. The shell prints each command
//! line and each output line also to stdout, with the prefix `shell: `.

use std::ffi::c_int;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::rc::Rc;
use std::sync::Arc;
use std::{env, fs, io, mem, thread};

use compositor_protocol::{
    DamageRequest, DamageResponse, DisplayInfoRequest, DisplayInfoResponse, FinishRequest,
    FinishResponse, NewWindowRequest, NewWindowResponse, Request, Response, call, connect,
};
use gfx::api::Rect;
use gfx::backend::virtio::{VirtioBackend, VirtioDevice, VirtioTexture};
use gfx::error::GfxError;
use libinput::{BACKSPACE, DeviceEvent, ENTER, Event, InputBuffer, InputDevices, Keyboard};
use shell_core::command::Command;
use shell_core::path::resolve;
use shell_core::terminal::{BACKGROUND, Terminal};
use shell_core::tree::{Entry, tree};

const INPUT_CAPACITY: NonZeroUsize = NonZeroUsize::new(256).unwrap();
/// One step of the mouse wheel moves the view by this number of text rows.
const WHEEL_STEP_ROWS: isize = 3;
/// The window size is `WINDOW_SHARE_NUMERATOR / WINDOW_SHARE_DENOMINATOR`
/// of the screen size, in each direction.
const WINDOW_SHARE_NUMERATOR: u32 = 3;
const WINDOW_SHARE_DENOMINATOR: u32 = 5;

fn gfx_error(e: GfxError) -> io::Error {
    io::Error::other(format!("gfx {e:?}"))
}

fn unexpected(response: Response) -> io::Error {
    io::Error::other(format!("unexpected response {response:?}"))
}

/// A compositor window that the CPU draws.
struct Window {
    stream: UnixStream,
    device: Rc<VirtioDevice>,
    texture: VirtioTexture,
}

impl Window {
    /// Asks the compositor for a window in the centre of the screen.
    fn open() -> io::Result<Self> {
        let stream = connect()?;
        let (screen_width, screen_height) =
            match call(&stream, &Request::DisplayInfo(DisplayInfoRequest {}))? {
                Response::DisplayInfo(DisplayInfoResponse { width, height }) => (width, height),
                other => return Err(unexpected(other)),
            };
        let width = screen_width * WINDOW_SHARE_NUMERATOR / WINDOW_SHARE_DENOMINATOR;
        let height = screen_height * WINDOW_SHARE_NUMERATOR / WINDOW_SHARE_DENOMINATOR;
        let (x, y) = ((screen_width - width) / 2, (screen_height - height) / 2);
        let fd = create_window(
            &stream,
            NewWindowRequest {
                x,
                y,
                width,
                height,
            },
        )?;
        println!("shell: window {width}x{height} at ({x},{y})");
        let device = VirtioDevice::open_render().map_err(gfx_error)?;
        let texture = VirtioBackend::new(device.clone())
            .import_texture_fd(fd, width, height)
            .map_err(gfx_error)?;
        Ok(Self {
            stream,
            device,
            texture,
        })
    }

    fn width(&self) -> usize {
        self.texture.width() as usize
    }

    fn height(&self) -> usize {
        self.texture.height() as usize
    }

    /// Uploads the full-width pixel rows `rows` and shows them on the screen.
    fn show(&self, rows: Range<usize>) -> io::Result<()> {
        let rect = Rect {
            x: 0,
            y: rows.start as u32,
            width: self.texture.width(),
            height: rows.len() as u32,
        };
        // The upload is complete when it returns, so no GPU work is pending
        // for the `Finish` below.
        self.device.upload(&self.texture, rect).map_err(gfx_error)?;
        let damage = DamageRequest {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        };
        match call(&self.stream, &Request::Damage(damage))? {
            Response::Damage(DamageResponse {}) => {}
            other => return Err(unexpected(other)),
        }
        match call(&self.stream, &Request::Finish(FinishRequest { fence: 0 }))? {
            Response::Finish(FinishResponse {}) => Ok(()),
            other => Err(unexpected(other)),
        }
    }
}

fn create_window(stream: &UnixStream, request: NewWindowRequest) -> io::Result<c_int> {
    match call(stream, &Request::NewWindow(request))? {
        Response::NewWindow(NewWindowResponse {}) => {}
        other => return Err(unexpected(other)),
    }
    muffin_sys::recv_fd(stream.as_raw_fd()).map_err(|e| io::Error::other(format!("recv_fd {e:?}")))
}

struct Shell {
    window: Window,
    terminal: Terminal,
    /// `chdir` is a stub, so the shell keeps the current directory itself.
    /// It is absolute and holds no `.` and no `..`.
    cwd: PathBuf,
    /// The text that the user typed after the prompt.
    line: String,
}

impl Shell {
    fn new(window: Window) -> io::Result<Self> {
        let terminal = Terminal::for_window(window.width(), window.height())
            .ok_or_else(|| io::Error::other("the window is too small for one cell"))?;
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut shell = Self {
            window,
            terminal,
            cwd,
            line: String::new(),
        };
        shell.window.texture.pixels_mut().fill(BACKGROUND);
        shell.prompt();
        shell.terminal.take_dirty();
        let (width, height) = (shell.window.width(), shell.window.height());
        shell.terminal.draw(
            0..shell.terminal.rows(),
            shell.window.texture.pixels_mut(),
            width,
        );
        shell.window.show(0..height)?;
        Ok(shell)
    }

    fn prompt_text(&self) -> String {
        format!("{}> ", self.cwd.display())
    }

    fn prompt(&mut self) {
        let prompt = self.prompt_text();
        self.terminal.write_str(&prompt);
    }

    /// Writes `text` and a new row to the terminal and to stdout.
    fn print(&mut self, text: &str) {
        println!("shell: {text}");
        self.terminal.write_str(text);
        self.terminal.put('\n');
    }

    /// Applies one character of the keyboard.
    fn key(&mut self, c: char) {
        match c {
            ENTER => self.enter(),
            BACKSPACE => {
                if self.line.pop().is_some() {
                    self.terminal.backspace();
                }
            }
            ' '..='~' => {
                self.line.push(c);
                self.terminal.put(c);
            }
            _ => {}
        }
    }

    fn enter(&mut self) {
        println!("shell: {}{}", self.prompt_text(), self.line);
        self.terminal.put('\n');
        let line = mem::take(&mut self.line);
        match Command::parse(&line) {
            Command::Empty => {}
            Command::Echo(text) => self.print(text),
            Command::Cd(path) => self.cd(path.unwrap_or("/")),
            Command::Ls(path) => self.ls(path.unwrap_or(".")),
            Command::Tree(path) => self.tree(path.unwrap_or(".")),
            Command::Poweroff => self.poweroff(),
            Command::Unknown(name) => self.print(&format!("unknown command: {name}")),
        }
        self.prompt();
    }

    fn cd(&mut self, path: &str) {
        let target = resolve(&self.cwd, path);
        match fs::metadata(&target) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return self.print(&format!("cd: {path}: not a directory")),
            Err(e) => return self.print(&format!("cd: {path}: {e}")),
        }
        match env::set_current_dir(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {}
            Err(e) => return self.print(&format!("cd: {path}: {e}")),
        }
        self.cwd = target;
    }

    fn ls(&mut self, path: &str) {
        match list_dir(&resolve(&self.cwd, path)) {
            Ok(entries) => {
                for Entry { name, is_dir } in entries {
                    let suffix = if is_dir { "/" } else { "" };
                    self.print(&format!("{name}{suffix}"));
                }
            }
            Err(e) => self.print(&format!("ls: {path}: {e}")),
        }
    }

    fn tree(&mut self, path: &str) {
        let root = resolve(&self.cwd, path);
        if let Err(e) = fs::read_dir(&root) {
            return self.print(&format!("tree: {path}: {e}"));
        }
        for line in tree(&root, &mut |dir| list_dir(dir)) {
            self.print(&line);
        }
    }

    fn poweroff(&mut self) -> ! {
        self.print("powering off");
        // The machine stops in any case, so a failed draw of the last line
        // does not matter.
        let _ = self.flush();
        muffin_sys::poweroff()
    }

    /// Moves the view by `steps` wheel steps. A positive value shows older
    /// output.
    fn scroll(&mut self, steps: i32) {
        let steps = isize::try_from(steps).expect("isize holds every i32 on x86-64");
        self.terminal.scroll(steps.saturating_mul(WHEEL_STEP_ROWS));
    }

    /// Draws and shows the text rows that changed.
    fn flush(&mut self) -> io::Result<()> {
        let Some(rows) = self.terminal.take_dirty() else {
            return Ok(());
        };
        let pixel_rows = Terminal::pixel_rows(&rows);
        let width = self.window.width();
        self.terminal
            .draw(rows, self.window.texture.pixels_mut(), width);
        self.window.show(pixel_rows)
    }
}

/// Returns the entries of `dir`, sorted by name.
fn list_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = fs::read_dir(dir)?
        .map(|entry| {
            let entry = entry?;
            Ok(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: entry.file_type().is_ok_and(|kind| kind.is_dir()),
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Starts the input thread. A failure of the thread ends the process.
fn start_input() -> io::Result<Arc<InputBuffer>> {
    let buffer = Arc::new(InputBuffer::new(INPUT_CAPACITY));
    let mut devices = InputDevices::open_all()?;
    let filled = Arc::clone(&buffer);
    thread::Builder::new()
        .name("input".to_owned())
        .spawn(move || {
            let Err(e) = devices.fill(&filled);
            println!("shell: FAIL input {e}");
            exit(1);
        })?;
    Ok(buffer)
}

fn run() -> io::Result<()> {
    let mut shell = Shell::new(Window::open()?)?;
    let input = start_input()?;
    let mut keyboard = Keyboard::new();
    println!("shell: ready");
    let mut events = Vec::new();
    loop {
        events.push(input.pop());
        input.drain(&mut events);
        for DeviceEvent { event, .. } in events.drain(..) {
            match event {
                Event::Wheel { delta } => shell.scroll(delta),
                event => {
                    if let Some(c) = keyboard.apply(event) {
                        shell.key(c);
                    }
                }
            }
        }
        shell.flush()?;
    }
}

fn main() {
    if let Err(e) = run() {
        println!("shell: FAIL {e}");
        exit(1);
    }
}
