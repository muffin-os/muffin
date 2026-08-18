use core::fmt::Write;

use kernel_log::{Environment, SpanStack};
use x86_64::instructions::interrupts;

use crate::hpet::hpet_maybe;
use crate::limine::EXECUTABLE_CMDLINE_REQUEST;
use crate::mcore::context::ExecutionContext;
use crate::serial;

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

    kernel_log::init::<KernelEnvironment>(text);
}

/// The kernel services behind the `kernel_log` subscriber: HPET time, CPU
/// identity, interrupt-safe critical sections, and the serial port sink.
struct KernelEnvironment;

impl Environment for KernelEnvironment {
    /// Nanoseconds since boot, or 0 while the HPET is not up yet.
    fn now_ns() -> u64 {
        hpet_maybe().map_or(0, |hpet| hpet.read().elapsed_ns())
    }

    fn critical<R>(f: impl FnOnce() -> R) -> R {
        interrupts::without_interrupts(f)
    }

    fn with_sink(f: impl FnOnce(&mut dyn Write)) {
        serial::with_serial(|serial| f(serial));
    }

    fn write_flow_label(out: &mut dyn Write) {
        if let Some(ctx) = ExecutionContext::try_load() {
            let _ = write!(out, "cpu{} pid{}", ctx.cpu_id(), ctx.pid());
        } else {
            let _ = write!(out, "boot");
        }
    }

    fn with_span_stack<R>(f: impl FnOnce(&mut SpanStack) -> R) -> Option<R> {
        let ctx = ExecutionContext::try_load()?;
        Some(f(&mut ctx.current_task().span_stack().lock()))
    }
}
