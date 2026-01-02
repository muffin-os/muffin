use jiff::{Timestamp, Unit};
use log::{Level, Metadata, Record};

use crate::hpet::hpet_maybe;
use crate::mcore::context::ExecutionContext;
use crate::serial_println;
use crate::time::TimestampExt;

pub(crate) fn init() {
    log::set_logger(&SerialLogger).unwrap();
    log::set_max_level(::log::LevelFilter::Trace);
}

pub struct SerialLogger;

impl SerialLogger {}

impl log::Log for SerialLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() < Level::Trace || metadata.target().starts_with("kernel")
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let timestamp = if hpet_maybe().is_some() {
                Timestamp::now()
            } else {
                Timestamp::new(0, 0).unwrap()
            }
            .round(Unit::Microsecond)
            .unwrap();

            if let Some(ctx) = ExecutionContext::try_load() {
                serial_println!(
                    "{} - {:5} cpu{} pid{:3} [{}] {}",
                    timestamp,
                    record.level(),
                    ctx.cpu_id(),
                    ctx.pid(),
                    record.target(),
                    record.args()
                );
            } else {
                serial_println!(
                    "{} - {:5} boot [{}] {}",
                    timestamp,
                    record.level(),
                    record.target(),
                    record.args()
                );
            }
        }
    }

    fn flush(&self) {
        // no-op
    }
}
