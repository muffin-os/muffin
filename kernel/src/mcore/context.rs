use alloc::sync::Arc;
use core::cell::UnsafeCell;

use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::KernelGsBase;
use x86_64::structures::gdt::GlobalDescriptorTable;
use x86_64::structures::idt::InterruptDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

use crate::arch::gdt::Selectors;
use crate::mcore::lapic::Lapic;
use crate::mcore::mtask::process::{Process, ProcessId};
use crate::mcore::mtask::scheduler::Scheduler;
use crate::mcore::mtask::task::Task;

#[derive(Debug)]
pub struct ExecutionContext {
    cpu_id: usize,
    lapic_id: usize,

    lapic: Mutex<Lapic>,

    _gdt: &'static GlobalDescriptorTable,
    sel: Selectors,
    _idt: &'static InterruptDescriptorTable,
    tss: *mut TaskStateSegment,

    scheduler: UnsafeCell<Scheduler>,
}

impl ExecutionContext {
    pub fn new(
        cpu: &limine::mp::Cpu,
        gdt: &'static GlobalDescriptorTable,
        sel: Selectors,
        idt: &'static InterruptDescriptorTable,
        tss: *mut TaskStateSegment,
        lapic: Lapic,
    ) -> Self {
        ExecutionContext {
            cpu_id: cpu.id as usize,
            lapic_id: cpu.lapic_id as usize,
            lapic: Mutex::new(lapic),
            _gdt: gdt,
            sel,
            _idt: idt,
            tss,
            scheduler: UnsafeCell::new(Scheduler::new_cpu_local()),
        }
    }

    /// Set the kernel stack pointer that the CPU will load into RSP on a Ring 3
    /// to Ring 0 privilege transition (e.g. on `int 0x80` from userspace).
    ///
    /// Must be updated on every context switch so each userspace task enters
    /// the kernel on its own per-task kernel stack rather than a shared one.
    pub fn set_kernel_stack(&self, top: VirtAddr) {
        // Safety: `tss` was obtained from a freshly leaked Box in
        // `create_gdt_and_tss` and is therefore valid for 'static. The TSS is
        // only ever written by this CPU's reschedule path (which holds the
        // scheduler mutably and runs with interrupts disabled), so there are
        // no concurrent accesses.
        unsafe {
            (*self.tss).privilege_stack_table[0] = top;
        }
    }

    #[must_use]
    pub fn try_load() -> Option<&'static Self> {
        let ctx = KernelGsBase::read();
        if ctx.is_null() {
            None
        } else {
            Some(unsafe { &*ctx.as_ptr() })
        }
    }

    /// # Panics
    /// This function panics if the execution context could not be loaded.
    /// This could happen if no execution context exists yet, or the pointer
    /// or its memory in `KernelGSBase` is invalid.
    #[must_use]
    pub fn load() -> &'static Self {
        Self::try_load().expect("could not load cpu context")
    }

    #[must_use]
    pub fn cpu_id(&self) -> usize {
        self.cpu_id
    }

    pub fn lapic_id(&self) -> usize {
        self.lapic_id
    }

    #[must_use]
    pub fn lapic(&self) -> &Mutex<Lapic> {
        &self.lapic
    }

    pub fn selectors(&self) -> &Selectors {
        &self.sel
    }

    /// Creates and returns a mutable reference to the scheduler.
    ///
    /// # Safety
    /// The caller must ensure that only one mutable reference
    /// to the scheduler exists at any time.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn scheduler_mut(&self) -> &mut Scheduler {
        unsafe { &mut *self.scheduler.get() }
    }

    pub fn scheduler(&self) -> &Scheduler {
        unsafe {
            // Safety: this is safe because either:
            // * there is a mutable reference that is used for rescheduling, in which case we are
            //   not currently executing this
            // * there is no mutable reference, in which case we are safe because we're not modifying
            // * someone else has a mutable reference, in which case he violates the safety contract
            //   if this is executed
            //
            // The above is true because everything in the context is cpu-local.
            &*self.scheduler.get()
        }
    }

    pub fn pid(&self) -> ProcessId {
        self.scheduler().current_task().process().pid()
    }

    pub fn current_task(&self) -> &Task {
        self.scheduler().current_task()
    }

    pub fn current_process(&self) -> &Arc<Process> {
        self.current_task().process()
    }
}
