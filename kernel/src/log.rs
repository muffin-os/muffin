use core::fmt::{Debug, Write};

use conquer_once::spin::OnceCell;
use kernel_log::Filter;
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Metadata, Subscriber, span};
use uart_16550::SerialPort;

use crate::hpet::hpet_maybe;
use crate::limine::EXECUTABLE_CMDLINE_REQUEST;
use crate::mcore::context::ExecutionContext;
use crate::serial;

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

struct SerialSubscriber;

impl Subscriber for SerialSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        filter().enabled(metadata.target(), metadata.level())
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(filter().max())
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
