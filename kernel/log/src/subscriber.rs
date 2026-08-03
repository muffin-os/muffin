//! A fully static, alloc-free tracing subscriber.
//!
//! The logging path must never allocate: in the kernel it runs in contexts
//! where the heap is unavailable (AP bring-up logs before the address-space
//! switch, so only statics from the kernel image are mapped) and in contexts
//! where an allocation can deadlock (an interrupt handler logging while the
//! interrupted flow holds the heap lock). Everything below is therefore
//! statically sized, and overflow degrades to truncation or untracked spans,
//! never to a fault.
//!
//! For the same reason [`SpanSubscriber`] must stay zero-sized: the
//! [`Dispatch`](tracing::Dispatch) holding it lives on the heap, and a ZST is
//! never read through that pointer.

use core::fmt::{Debug, Write};
use core::marker::PhantomData;

use conquer_once::spin::OnceCell;
use spin::Mutex;
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Metadata, Subscriber, span};

use crate::Filter;

/// Live spans tracked at once. Spans created while the pool is full are
/// handed an id but not tracked, so they render no close record.
const MAX_SPANS: usize = 64;
/// Rendered fields per span, truncating beyond.
const FIELDS_CAP: usize = 256;
/// Rendered close label per span, truncating beyond.
const LABEL_CAP: usize = 256;

static FILTER: OnceCell<Filter> = OnceCell::uninit();

/// All live span data. See [`SpanPool`].
static SPAN_POOL: Mutex<SpanPool> = Mutex::new(SpanPool::new());

/// The services a [`SpanSubscriber`] needs from its surroundings.
///
/// All functions are associated so the subscriber stays zero-sized. Every
/// implementation must be callable from any context the environment can log
/// from, including interrupt handlers.
pub trait Environment: 'static {
    /// Nanoseconds on a monotonic clock, or 0 while none is available yet.
    fn now_ns() -> u64;

    /// Runs `f` with this flow unpreemptible (in the kernel: interrupts
    /// disabled), so a log call from an interrupt handler cannot deadlock
    /// against a subscriber lock held by the interrupted code.
    fn critical<R>(f: impl FnOnce() -> R) -> R;

    /// Runs `f` on the output sink, exclusively for one whole record.
    fn with_sink(f: impl FnOnce(&mut dyn Write));

    /// Writes the flow label of a record prefix (in the kernel:
    /// `cpu0 pid0` or `boot`).
    fn write_flow_label(out: &mut dyn Write);
}

/// Parses `rust_log` into the global filter and installs the subscriber.
///
/// # Panics
/// Panics if called twice.
pub fn init<E: Environment>(rust_log: &str) {
    let filter = Filter::parse(rust_log);
    FILTER.init_once(|| filter);

    tracing::dispatcher::set_global_default(tracing::Dispatch::new(SpanSubscriber::<E>::new()))
        .expect("tracing subscriber cannot be set twice");
}

fn filter() -> &'static Filter {
    FILTER.try_get().expect("log filter is not initialized")
}

/// Runs `f` against the span pool inside a critical section.
fn with_spans<E: Environment, R>(f: impl FnOnce(&mut SpanPool) -> R) -> R {
    E::critical(|| f(&mut SPAN_POOL.lock()))
}

/// A fixed-capacity UTF-8 buffer that silently truncates at char boundaries.
struct FixedBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> FixedBuf<N> {
    const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        // Only whole `str` prefixes ending at char boundaries are ever
        // appended, so this cannot fail.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Write for FixedBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let remaining = N - self.len;
        let mut take = s.len().min(remaining);
        while take > 0 && !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// A fixed pool of live span data, keyed by monotonically increasing span
/// ids and searched linearly, which is cheap at logging frequency.
///
/// Each span lives from `new_span` until its last reference is closed.
/// Timing follows the tracing-subscriber model: `busy` accumulates
/// enter..exit intervals, `idle` the gaps in between, `last` is the instant
/// of the most recent transition. Both are reported once, when the span
/// closes.
struct SpanPool {
    slots: [Option<SpanData>; MAX_SPANS],
    next_id: u64,
}

struct SpanData {
    id: u64,
    meta: &'static Metadata<'static>,
    /// Fields captured at creation and via `record`, pre-rendered as dim
    /// `key=value` pairs with a leading space.
    fields: FixedBuf<FIELDS_CAP>,
    refs: usize,
    busy: u64,
    idle: u64,
    last: u64,
}

impl SpanData {
    /// Renders `name(fields)`, the fields dim-parenthesized and only if present.
    fn write_label(&self, out: &mut impl Write) {
        let _ = write!(out, "{}", self.meta.name());
        if !self.fields.is_empty() {
            let _ = write!(
                out,
                "\x1b[2m(\x1b[0m{}\x1b[2m)\x1b[0m",
                self.fields.as_str().trim_start()
            );
        }
    }
}

impl SpanPool {
    const fn new() -> Self {
        Self {
            slots: [const { None }; MAX_SPANS],
            next_id: 1,
        }
    }

    fn insert(&mut self, data: SpanData) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(data);
        }
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut SpanData> {
        self.slots.iter_mut().flatten().find(|data| data.id == id)
    }

    fn remove(&mut self, id: u64) {
        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|data| data.id == id))
        {
            *slot = None;
        }
    }
}

/// A [`Subscriber`] rendering events and span close records through an
/// [`Environment`]. Zero-sized by construction, see the module docs.
struct SpanSubscriber<E: Environment> {
    _env: PhantomData<fn() -> E>,
}

impl<E: Environment> SpanSubscriber<E> {
    fn new() -> Self {
        Self { _env: PhantomData }
    }
}

impl<E: Environment> Subscriber for SpanSubscriber<E> {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        filter().enabled(metadata.target(), metadata.level())
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(filter().max())
    }

    fn new_span(&self, span: &span::Attributes<'_>) -> span::Id {
        let mut fields = FixedBuf::new();
        span.record(&mut FieldVisitor { out: &mut fields });
        let meta = span.metadata();

        let now = E::now_ns();
        let id = with_spans::<E, _>(|pool| {
            let id = pool.next_id;
            pool.next_id = pool.next_id.wrapping_add(1);
            if pool.next_id == 0 {
                pool.next_id = 1;
            }
            pool.insert(SpanData {
                id,
                meta,
                fields,
                refs: 1,
                busy: 0,
                idle: 0,
                last: now,
            });
            id
        });
        span::Id::from_u64(id)
    }

    fn record(&self, span: &span::Id, values: &span::Record<'_>) {
        with_spans::<E, _>(|pool| {
            if let Some(data) = pool.get_mut(span.into_u64()) {
                values.record(&mut FieldVisitor {
                    out: &mut data.fields,
                });
            }
        });
    }

    // Not rendered, matching tracing-subscriber's fmt layer, which stores but
    // never displays follows-from links either.
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        write_record::<E>(meta.level(), meta.target(), |out| {
            event.record(&mut MessageVisitor { out: &mut *out });
            event.record(&mut FieldVisitor { out: &mut *out });
        });
    }

    fn clone_span(&self, span: &span::Id) -> span::Id {
        with_spans::<E, _>(|pool| {
            if let Some(data) = pool.get_mut(span.into_u64()) {
                data.refs += 1;
            }
        });
        span.clone()
    }

    fn enter(&self, span: &span::Id) {
        let id = span.into_u64();
        let now = E::now_ns();
        with_spans::<E, _>(|pool| {
            if let Some(data) = pool.get_mut(id) {
                data.idle += now.saturating_sub(data.last);
                data.last = now;
            }
        });
    }

    fn exit(&self, span: &span::Id) {
        let id = span.into_u64();
        let now = E::now_ns();
        with_spans::<E, _>(|pool| {
            if let Some(data) = pool.get_mut(id) {
                data.busy += now.saturating_sub(data.last);
                data.last = now;
            }
        });
    }

    fn try_close(&self, span: span::Id) -> bool {
        let id = span.into_u64();
        let now = E::now_ns();

        // The label is rendered under the lock into a stack buffer. The sink
        // write happens unlocked.
        let mut label = FixedBuf::<LABEL_CAP>::new();
        let closed = with_spans::<E, _>(|pool| {
            let data = pool.get_mut(id)?;
            data.refs = data.refs.saturating_sub(1);
            if data.refs > 0 {
                return None;
            }
            data.idle += now.saturating_sub(data.last);
            let (meta, busy, idle) = (data.meta, data.busy, data.idle);

            data.write_label(&mut label);
            pool.remove(id);
            Some((meta, busy, idle))
        });

        if let Some((meta, busy, idle)) = closed {
            write_record::<E>(meta.level(), meta.target(), |out| {
                let _ = write!(out, "{} \x1b[2mdone\x1b[0m", label.as_str());
                let _ = write!(out, " \x1b[2mbusy=\x1b[0m");
                write_duration(out, busy);
                let _ = write!(out, " \x1b[2midle=\x1b[0m");
                write_duration(out, idle);
            });
            true
        } else {
            false
        }
    }
}

/// Writes only the `message` field into `out`.
struct MessageVisitor<'a, W: Write + ?Sized> {
    out: &'a mut W,
}

impl<W: Write + ?Sized> Visit for MessageVisitor<'_, W> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            let _ = write!(self.out, "{value}");
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "message" {
            let _ = write!(self.out, "{value:?}");
        }
    }
}

/// Writes every field except `message` into `out` as a dim `key=value` pair.
struct FieldVisitor<'a, W: Write + ?Sized> {
    out: &'a mut W,
}

impl<W: Write + ?Sized> Visit for FieldVisitor<'_, W> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() != "message" {
            let _ = write!(self.out, " \x1b[2m{}=\x1b[0m{value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() != "message" {
            let _ = write!(self.out, " \x1b[2m{}=\x1b[0m{value:?}", field.name());
        }
    }
}

/// Writes `ns` in the shortest sensible unit with two decimals (`842ns`,
/// `1.20µs`, `13.05ms`, `2.50s`). Integer math only.
fn write_duration(out: &mut (impl Write + ?Sized), ns: u64) {
    let (whole, frac, unit) = if ns < 1_000 {
        let _ = write!(out, "{ns}ns");
        return;
    } else if ns < 1_000_000 {
        (ns / 1_000, (ns % 1_000) / 10, "µs")
    } else if ns < 1_000_000_000 {
        (ns / 1_000_000, (ns % 1_000_000) / 10_000, "ms")
    } else {
        (ns / 1_000_000_000, (ns % 1_000_000_000) / 10_000_000, "s")
    };
    let _ = write!(out, "{whole}.{frac:02}{unit}");
}

/// Writes one whole record inside a single sink acquisition.
///
/// `body` writes the message and any extra fields after the fixed prefix.
fn write_record<E: Environment>(level: &Level, target: &str, body: impl FnOnce(&mut dyn Write)) {
    let ns = E::now_ns();
    let secs = ns / 1_000_000_000;
    let micros = (ns % 1_000_000_000) / 1_000;
    let (color, name) = level_style(level);

    E::with_sink(|out| {
        let _ = write!(
            out,
            "\x1b[2m[{secs:>5}.{micros:06}]\x1b[0m {color}{name:<5}\x1b[0m "
        );
        E::write_flow_label(out);
        let _ = write!(out, " \x1b[2m{target}:\x1b[0m ");
        body(out);
        let _ = writeln!(out);
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

#[cfg(test)]
mod tests {
    extern crate std;

    use core::fmt::Write;
    use core::sync::atomic::AtomicU64;
    use core::sync::atomic::Ordering::Relaxed;
    use std::string::String;

    use spin::Mutex;

    use super::{Environment, FILTER, FixedBuf, SpanSubscriber, write_duration};
    use crate::Filter;

    #[test]
    fn fixed_buf_truncates_at_char_boundary() {
        let mut buf = FixedBuf::<3>::new();
        // 'µ' is 2 bytes. Only one of them fits, so the whole char is dropped.
        let _ = write!(buf, "abµ");
        assert_eq!(buf.as_str(), "ab", "partial 'µ' must not be split");

        let mut exact = FixedBuf::<4>::new();
        let _ = write!(exact, "abcd");
        assert_eq!(exact.as_str(), "abcd", "exact fit is kept whole");
        assert!(!exact.is_empty(), "filled buffer is not empty");
    }

    #[test]
    fn duration_units() {
        let mut out = String::new();
        for (ns, expected) in [
            (0, "0ns"),
            (999, "999ns"),
            (1_000, "1.00µs"),
            (1_204, "1.20µs"),
            (13_050_000, "13.05ms"),
            (2_500_000_000, "2.50s"),
        ] {
            out.clear();
            write_duration(&mut out, ns);
            assert_eq!(out, expected, "wrong rendering for {ns}ns");
        }
    }

    // The environment of the `span_lifecycle` test. Time advances 1µs per
    // reading so busy/idle are nonzero and deterministic.
    struct MockEnv;

    static NOW: AtomicU64 = AtomicU64::new(0);
    static SINK: Mutex<String> = Mutex::new(String::new());

    impl Environment for MockEnv {
        fn now_ns() -> u64 {
            NOW.fetch_add(1_000, Relaxed) + 1_000
        }

        fn critical<R>(f: impl FnOnce() -> R) -> R {
            f()
        }

        fn with_sink(f: impl FnOnce(&mut dyn Write)) {
            f(&mut *SINK.lock());
        }

        fn write_flow_label(out: &mut dyn Write) {
            let _ = write!(out, "test");
        }
    }

    #[test]
    fn span_lifecycle() {
        let _ = FILTER.try_init_once(|| Filter::parse("trace"));

        tracing::subscriber::set_global_default(SpanSubscriber::<MockEnv>::new())
            .expect("no other test sets a subscriber");
        let outer = tracing::info_span!("outer", answer = 42);
        {
            let _outer_guard = outer.enter();
            let inner = tracing::info_span!("inner");
            let _inner_guard = inner.enter();
            tracing::info!("hello");
        }
        drop(outer);

        let out = SINK.lock();
        let hello = out.find("hello").expect("event message rendered");
        let done = out.find("done").expect("close records rendered");
        assert!(
            done > hello,
            "no span record before close, output:\n{}",
            *out
        );
        assert_eq!(out.matches("done").count(), 2, "one close record per span");
        assert!(
            out.contains("answer=") && out.contains("42"),
            "span fields rendered, output:\n{}",
            *out
        );
        assert!(
            out.contains("busy=") && out.contains("idle="),
            "close records carry timing, output:\n{}",
            *out
        );
        // Events are plain records. Span names never prefix them.
        let hello_line = out
            .lines()
            .find(|line| line.contains("hello"))
            .expect("event line present");
        assert!(
            !hello_line.contains("outer") && !hello_line.contains("inner"),
            "event lines carry no span scope, output:\n{}",
            *out
        );
    }
}
