use core::fmt::{Debug, Write};

use conquer_once::spin::OnceCell;
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Metadata, Subscriber, span};
use uart_16550::SerialPort;

use crate::hpet::hpet_maybe;
use crate::limine::EXECUTABLE_CMDLINE_REQUEST;
use crate::mcore::context::ExecutionContext;
use crate::serial;

const MAX_DIRECTIVES: usize = 16;
const BUF_SIZE: usize = 256;

static FILTER: OnceCell<Filter> = OnceCell::uninit();

pub(crate) fn init() {
    let text = EXECUTABLE_CMDLINE_REQUEST
        .get_response()
        .and_then(|resp| resp.cmdline().to_str().ok())
        .and_then(|cmdline| {
            cmdline
                .split_ascii_whitespace()
                .filter_map(|token| token.strip_prefix("RUST_LOG="))
                .next_back()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or("info");

    let filter = Filter::parse(text);
    FILTER.init_once(|| filter);

    tracing::dispatcher::set_global_default(tracing::Dispatch::new(SerialSubscriber))
        .expect("tracing subscriber cannot be set twice");
}

fn filter() -> &'static Filter {
    FILTER.try_get().expect("log filter is not initialized")
}

/// A directive is a target prefix mapped to a level.
///
/// An empty span (`start == end`) is the global directive matching every target.
#[derive(Copy, Clone)]
struct Directive {
    start: u16,
    end: u16,
    level: LevelFilter,
}

/// An env_logger style filter parsed once into a fixed buffer, no allocation.
struct Filter {
    buf: [u8; BUF_SIZE],
    directives: [Directive; MAX_DIRECTIVES],
    count: usize,
    max: LevelFilter,
}

impl Filter {
    fn parse(input: &str) -> Self {
        let mut buf = [0u8; BUF_SIZE];
        let len = input.len().min(BUF_SIZE);
        buf[..len].copy_from_slice(&input.as_bytes()[..len]);

        let mut directives = [Directive {
            start: 0,
            end: 0,
            level: LevelFilter::OFF,
        }; MAX_DIRECTIVES];
        let mut count = 0;
        let mut max = LevelFilter::OFF;

        let mut pos = 0;
        while pos < len && count < MAX_DIRECTIVES {
            let seg_end = buf[pos..len]
                .iter()
                .position(|&c| c == b',')
                .map_or(len, |i| pos + i);
            if let Some(directive) = parse_directive(&buf, pos, seg_end) {
                if directive.level > max {
                    max = directive.level;
                }
                directives[count] = directive;
                count += 1;
            }
            pos = seg_end + 1;
        }

        Self {
            buf,
            directives,
            count,
            max,
        }
    }

    fn enabled(&self, target: &str, level: &Level) -> bool {
        let mut best: Option<LevelFilter> = None;
        let mut best_len = 0;
        for directive in &self.directives[..self.count] {
            let prefix = &self.buf[directive.start as usize..directive.end as usize];
            if target.as_bytes().starts_with(prefix) && (best.is_none() || prefix.len() >= best_len)
            {
                best = Some(directive.level);
                best_len = prefix.len();
            }
        }
        best.is_some_and(|allowed| allowed >= *level)
    }
}

fn parse_directive(buf: &[u8], start: usize, end: usize) -> Option<Directive> {
    let (start, end) = trim(buf, start, end);
    if start == end {
        return None;
    }

    if let Some(eq) = buf[start..end]
        .iter()
        .position(|&c| c == b'=')
        .map(|i| start + i)
    {
        let (ts, te) = trim(buf, start, eq);
        let (ls, le) = trim(buf, eq + 1, end);
        let level = parse_level(&buf[ls..le])?;
        Some(Directive {
            start: ts as u16,
            end: te as u16,
            level,
        })
    } else if let Some(level) = parse_level(&buf[start..end]) {
        Some(Directive {
            start: start as u16,
            end: start as u16,
            level,
        })
    } else {
        Some(Directive {
            start: start as u16,
            end: end as u16,
            level: LevelFilter::TRACE,
        })
    }
}

fn trim(buf: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && buf[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && buf[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}

fn parse_level(s: &[u8]) -> Option<LevelFilter> {
    if s.eq_ignore_ascii_case(b"off") {
        Some(LevelFilter::OFF)
    } else if s.eq_ignore_ascii_case(b"error") {
        Some(LevelFilter::ERROR)
    } else if s.eq_ignore_ascii_case(b"warn") {
        Some(LevelFilter::WARN)
    } else if s.eq_ignore_ascii_case(b"info") {
        Some(LevelFilter::INFO)
    } else if s.eq_ignore_ascii_case(b"debug") {
        Some(LevelFilter::DEBUG)
    } else if s.eq_ignore_ascii_case(b"trace") {
        Some(LevelFilter::TRACE)
    } else {
        None
    }
}

struct SerialSubscriber;

impl Subscriber for SerialSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        filter().enabled(metadata.target(), metadata.level())
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(filter().max)
    }

    // Spans are unused in this kernel, so span operations are deliberate no-ops.
    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        write_record(meta.level(), meta.target(), |serial| {
            event.record(&mut MessageVisitor {
                serial: &mut *serial,
            });
            event.record(&mut FieldVisitor {
                serial: &mut *serial,
            });
        });
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}
}

/// Writes only the `message` field into the serial port.
struct MessageVisitor<'a> {
    serial: &'a mut SerialPort,
}

impl Visit for MessageVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            let _ = write!(self.serial, "{value}");
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "message" {
            let _ = write!(self.serial, "{value:?}");
        }
    }
}

/// Writes every field except `message` as a dim `key=value` pair.
struct FieldVisitor<'a> {
    serial: &'a mut SerialPort,
}

impl Visit for FieldVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() != "message" {
            let _ = write!(self.serial, " \x1b[2m{}=\x1b[0m{value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() != "message" {
            let _ = write!(self.serial, " \x1b[2m{}=\x1b[0m{value:?}", field.name());
        }
    }
}

/// Writes one whole record inside a single serial lock acquisition.
///
/// `body` writes the message and any extra fields after the fixed prefix.
fn write_record(level: &Level, target: &str, body: impl FnOnce(&mut SerialPort)) {
    let ns = hpet_maybe().map_or(0, |hpet| hpet.read().main_counter_value());
    let secs = ns / 1_000_000_000;
    let micros = (ns % 1_000_000_000) / 1_000;
    let (color, name) = level_style(level);

    serial::with_serial(|serial| {
        let _ = write!(
            serial,
            "\x1b[2m[{secs:>5}.{micros:06}]\x1b[0m {color}{name:<5}\x1b[0m "
        );
        if let Some(ctx) = ExecutionContext::try_load() {
            let _ = write!(serial, "cpu{} pid{}", ctx.cpu_id(), ctx.pid());
        } else {
            let _ = write!(serial, "boot");
        }
        let _ = write!(serial, " \x1b[2m{target}:\x1b[0m ");
        body(serial);
        let _ = writeln!(serial);
    });
}

fn level_style(level: &Level) -> (&'static str, &'static str) {
    if *level == Level::ERROR {
        ("\x1b[1;31m", "ERROR")
    } else if *level == Level::WARN {
        ("\x1b[33m", "WARN")
    } else if *level == Level::INFO {
        ("\x1b[32m", "INFO")
    } else if *level == Level::DEBUG {
        ("\x1b[34m", "DEBUG")
    } else {
        ("\x1b[35m", "TRACE")
    }
}
