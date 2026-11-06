use alloc::boxed::Box;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::Ordering::{Acquire, Release};

use tracing::{Level, instrument, trace};
use x86_64::instructions::interrupts;
use x86_64::instructions::segmentation::{CS, DS, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::registers::model_specific::KernelGsBase;
use x86_64::registers::segmentation::Segment;
use x86_64::structures::paging::PhysFrame;
use x86_64::{PhysAddr, VirtAddr};

use crate::arch::gdt::create_gdt_and_tss;
use crate::arch::idt::create_idt;
use crate::limine::MP_REQUEST;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::scheduler::cleanup::TaskCleanup;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::scheduler::idle;
use crate::mcore::mtask::task::Task;
use crate::sse;

pub mod context;
mod lapic;
pub mod mtask;

#[expect(clippy::missing_panics_doc)]
#[instrument(name = "init multitasking", level = Level::DEBUG)]
pub fn init() {
    let resp = unsafe {
        #[expect(static_mut_refs)] // we need this to set the `extra` field in the CPU structs
        MP_REQUEST.get_response_mut()
    }
    .unwrap();

    let cr3_val = {
        let (frame, flags) = Cr3::read();
        frame.start_address().as_u64() | flags.bits()
    };

    resp.cpus().iter().for_each(|cpu| {
        cpu.extra.store(cr3_val, Release);
    });

    GlobalTaskQueue::init();

    TaskCleanup::init();

    resp.cpus().iter().skip(1).for_each(|cpu| {
        cpu.goto_address.write(cpu_init);
    });

    unsafe { cpu_init_and_return(resp.cpus()[0]) };

    // the timer is running and could trigger an interrupt,
    // so the idle task must exist before `sti`
    install_idle_task();
    interrupts::enable();
}

#[instrument(name = "init cpu", level = Level::INFO, fields(cpu = cpu.id))]
unsafe extern "C" fn cpu_init_and_return(cpu: &limine::mp::Cpu) {
    let cpu_arg = cpu.extra.load(Acquire);
    trace!("booting cpu {} with argument {}", cpu.id, cpu_arg);

    unsafe {
        let flags = Cr3Flags::from_bits_truncate(cpu_arg);
        Cr3::write(PhysFrame::containing_address(PhysAddr::new(cpu_arg)), flags);
    }

    let (gdt, sel, tss) = create_gdt_and_tss();
    let gdt = Box::leak(Box::new(gdt));
    gdt.load();
    unsafe {
        CS::set_reg(sel.kernel_code);
        DS::set_reg(sel.kernel_data);
        SS::set_reg(sel.kernel_data);
        load_tss(sel.tss);
    }

    let idt = create_idt();
    let idt = Box::leak(Box::new(idt));
    idt.load();

    let lapic = lapic::init();

    {
        let ctx = ExecutionContext::new(cpu, gdt, sel, idt, tss, lapic);
        let addr = VirtAddr::from_ptr(Box::leak(Box::new(ctx)));
        KernelGsBase::write(addr);
    }

    sse::init();
}

unsafe extern "C" fn cpu_init(cpu: &limine::mp::Cpu) -> ! {
    unsafe { cpu_init_and_return(cpu) };

    install_idle_task();

    Task::exit_current()
}

fn install_idle_task() {
    assert!(
        !interrupts::are_enabled(),
        "interrupts must be disabled, since the interrupt tasks must exist before timer interrupts can arrive"
    );
    let idle_task = Task::create_new(Process::root(), idle, ptr::null_mut())
        .expect("should be able to create the idle task");
    // Safety: interrupts are disabled, so the context is this CPU's
    // while the idle task is installed.
    let context = unsafe { ExecutionContext::load() };
    unsafe { context.scheduler_mut() }.set_idle_task(Box::pin(idle_task));
    let lapic_id = u32::try_from(context.lapic_id()).expect("LAPIC ID should fit in u32");
    idle::register(context.cpu_id(), lapic_id);
}

extern "C" fn idle(_: *mut c_void) {
    loop {
        interrupts::disable();
        // Safety: interrupts are off, so the context is this CPU's.
        let context = unsafe { ExecutionContext::load() };
        let cpu_id = context.cpu_id();
        idle::mark_idle(cpu_id);
        // A switch can resume this task much later than its queue check, so
        // the loop checks the queue again before any halt.
        // Safety: interrupts are off, so the scheduler is this CPU's.
        if unsafe { context.scheduler_mut().reschedule() } {
            continue;
        }
        // An IPI after `mark_idle` stays pending until `sti` takes effect.
        interrupts::enable_and_hlt();
        interrupts::disable();
        idle::mark_active(cpu_id);
    }
}

pub fn exit_bootstrap() -> ! {
    Task::exit_current()
}
