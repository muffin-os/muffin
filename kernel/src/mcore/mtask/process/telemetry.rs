use core::sync::atomic::AtomicUsize;

#[derive(Default)]
pub struct Telemetry {
    pub page_faults: AtomicUsize,
}
