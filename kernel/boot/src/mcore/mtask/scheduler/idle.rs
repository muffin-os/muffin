//! This module records which CPUs halt in their idle task, so that an
//! enqueue can wake one of them with an IPI.

use alloc::vec::Vec;

use crate::sync::IrqMutex;

#[derive(Debug)]
struct CpuEntry {
    cpu_id: usize,
    lapic_id: u32,
    is_idle: bool,
}

/// This list is sorted by CPU id.
static CPUS: IrqMutex<Vec<CpuEntry>> = IrqMutex::new(Vec::new());

/// Registers a CPU as an active notification target.
///
/// # Panics
/// Panics if `cpu_id` is already registered.
pub(in crate::mcore) fn register(cpu_id: usize, lapic_id: u32) {
    let mut cpus = CPUS.lock_without_interrupts();
    let index = cpus.partition_point(|e| e.cpu_id < cpu_id);
    assert!(
        cpus.get(index).is_none_or(|e| e.cpu_id != cpu_id),
        "cpu {cpu_id} is already registered for idle notification"
    );
    cpus.insert(
        index,
        CpuEntry {
            cpu_id,
            lapic_id,
            is_idle: false,
        },
    );
}

fn set_idle(cpu_id: usize, is_idle: bool) {
    let mut cpus = CPUS.lock_without_interrupts();
    if let Ok(index) = cpus.binary_search_by_key(&cpu_id, |e| e.cpu_id) {
        cpus[index].is_idle = is_idle;
    }
}

pub(in crate::mcore) fn mark_idle(cpu_id: usize) {
    set_idle(cpu_id, true);
}

pub(in crate::mcore) fn mark_active(cpu_id: usize) {
    set_idle(cpu_id, false);
}

/// Claims one idle CPU other than `excluding_cpu` and returns its LAPIC ID.
/// The claim marks that CPU active, so one idle interval gets one IPI.
pub(in crate::mcore) fn claim_idle(excluding_cpu: usize) -> Option<u32> {
    let mut cpus = CPUS.lock_without_interrupts();
    let entry = cpus
        .iter_mut()
        .find(|e| e.is_idle && e.cpu_id != excluding_cpu)?;
    entry.is_idle = false;
    Some(entry.lapic_id)
}
