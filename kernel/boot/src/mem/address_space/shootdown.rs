//! This module invalidates the TLB entries of other CPUs through an IPI.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::hint::spin_loop;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Release};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};

use spin::Mutex;
use x86_64::instructions::{interrupts, tlb};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::PhysFrame;

use crate::arch::idt::InterruptIndex;
use crate::mcore::context::ExecutionContext;

/// This lock serializes the initiators. One target and one ack counter are
/// then sufficient. The lock does not guard them. The IPI handler on each
/// other CPU reads the target and decrements the counter while the
/// initiator holds this lock. A lock in the handler would deadlock both CPUs.
static IN_PROGRESS: Mutex<()> = Mutex::new(());
/// This list holds every CPU that loaded its IDT and enabled its LAPIC. It
/// is sorted by CPU id.
static ONLINE: Mutex<Vec<Arc<CpuSlot>>> = Mutex::new(Vec::new());
/// The initiator writes the start address of the target level 4 table here.
/// The handlers read it.
static TARGET_LEVEL4: AtomicU64 = AtomicU64::new(0);
/// The initiator writes the target count here. Each handler decrements it
/// one time.
static PENDING_ACKS: AtomicUsize = AtomicUsize::new(0);

/// A `CpuSlot` holds the shootdown state of one CPU.
#[derive(Debug)]
pub struct CpuSlot {
    cpu_id: usize,
    lapic_id: u32,
    /// The initiator sets this flag before it sends the IPI to this CPU. A
    /// CPU that finds the flag clear does not acknowledge.
    flush_pending: AtomicBool,
}

impl CpuSlot {
    /// Registers the calling CPU as a shootdown target. The CPU must be
    /// able to take the shootdown IPI from this point on.
    ///
    /// # Panics
    /// Panics if `cpu_id` is already registered.
    pub fn register(cpu_id: usize, lapic_id: u32) -> Arc<Self> {
        let slot = Arc::new(Self {
            cpu_id,
            lapic_id,
            flush_pending: AtomicBool::new(false),
        });
        let mut online = ONLINE.lock();
        let index = online.partition_point(|s| s.cpu_id < cpu_id);
        assert!(
            online.get(index).is_none_or(|s| s.cpu_id != cpu_id),
            "cpu {cpu_id} registered twice"
        );
        online.insert(index, slot.clone());
        slot
    }

    pub fn cpu_id(&self) -> usize {
        self.cpu_id
    }

    pub fn lapic_id(&self) -> u32 {
        self.lapic_id
    }

    /// Flushes this CPU's TLB when the active address space is the
    /// shootdown target, then acknowledges the shootdown. Call it from the
    /// IPI handler with interrupts off.
    pub fn acknowledge(&self) {
        if !self.flush_pending.swap(false, AcqRel) {
            return;
        }
        let active = Cr3::read().0.start_address().as_u64();
        if active == TARGET_LEVEL4.load(Acquire) {
            tlb::flush_all();
        }
        PENDING_ACKS.fetch_sub(1, AcqRel);
    }
}

/// Invalidates the TLB entries of `level4_frame` on every other online CPU
/// and returns after every one acknowledged. The calling CPU does not
/// flush. The local mapper flushes on each change.
///
/// Interrupts must be enabled. A concurrent initiator on another CPU then
/// receives this CPU's acknowledgement while this CPU waits.
///
/// # Panics
/// Panics if interrupts are disabled.
pub(super) fn flush_on_all_cpus(level4_frame: PhysFrame) {
    assert!(interrupts::are_enabled());
    let _serialized = IN_PROGRESS.lock();
    TARGET_LEVEL4.store(level4_frame.start_address().as_u64(), Release);

    let pending = interrupts::without_interrupts(|| {
        // Safety: interrupts are off, so the context stays this CPU's during
        // the flag setup and the IPIs.
        let context = unsafe { ExecutionContext::load() };
        let own = context.shootdown_slot().cpu_id();
        let online = ONLINE.lock();
        let targets = online.iter().filter(|slot| slot.cpu_id != own);
        PENDING_ACKS.store(targets.clone().count(), Release);
        let mut lapic = context.lapic().lock();
        let mut count = 0;
        for slot in targets {
            slot.flush_pending.store(true, Release);
            // Safety: interrupts are off, so the LAPIC is this CPU's.
            unsafe { lapic.send_ipi(InterruptIndex::TlbShootdown.as_u8(), slot.lapic_id) };
            count += 1;
        }
        count
    });

    while pending > 0 && PENDING_ACKS.load(Acquire) > 0 {
        spin_loop();
    }
}
