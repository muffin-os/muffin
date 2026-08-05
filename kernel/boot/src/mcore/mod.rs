use alloc::boxed::Box;
use core::sync::atomic::Ordering::{Acquire, Release};

use tracing::{Level, info, instrument, trace};
use x86_64::instructions::segmentation::{CS, DS, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::instructions::{hlt, interrupts};
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::registers::model_specific::KernelGsBase;
use x86_64::registers::segmentation::Segment;
use x86_64::structures::paging::PhysFrame;
use x86_64::{PhysAddr, VirtAddr};

use crate::apic::io_apic;
use crate::arch::gdt::create_gdt_and_tss;
use crate::arch::idt::create_idt;
use crate::limine::MP_REQUEST;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::scheduler::cleanup::TaskCleanup;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::scheduler::stopped::StoppedTasks;
use crate::sse;

pub mod context;
mod lapic;
pub mod mtask;

#[allow(clippy::missing_panics_doc)]
#[instrument(level = Level::DEBUG)]
pub fn init() {
    let resp = unsafe {
        #[allow(static_mut_refs)] // we need this to set the `extra` field in the CPU structs
        MP_REQUEST.get_response_mut()
    }
    .unwrap();

    let cr3_val = {
        let (frame, flags) = Cr3::read();
        frame.start_address().as_u64() | flags.bits()
    };

    // set the extra field in the CPU structs to the CR3 value
    resp.cpus().iter().for_each(|cpu| {
        cpu.extra.store(cr3_val, Release);
    });

    GlobalTaskQueue::init();
    StoppedTasks::init();

    // then call the `cpu_init` function on each CPU (no-op on bootstrap CPU)
    resp.cpus().iter().skip(1).for_each(|cpu| {
        cpu.goto_address.write(cpu_init_and_idle);
    });

    // then call the `cpu_init` function on the bootstrap CPU
    unsafe { cpu_init_and_return(resp.cpus()[0]) };

    TaskCleanup::init();
}

unsafe extern "C" fn cpu_init_and_return(cpu: &limine::mp::Cpu) {
    let cpu_arg = cpu.extra.load(Acquire);
    trace!("booting cpu {} with argument {}", cpu.id, cpu_arg,);

    // set the memory mapping that we got as a parameter
    unsafe {
        let flags = Cr3Flags::from_bits_truncate(cpu_arg);
        Cr3::write(PhysFrame::containing_address(PhysAddr::new(cpu_arg)), flags);
    }

    // set up the GDT
    let (gdt, sel, tss) = create_gdt_and_tss();
    let gdt = Box::leak(Box::new(gdt));
    gdt.load();
    unsafe {
        CS::set_reg(sel.kernel_code);
        DS::set_reg(sel.kernel_data);
        SS::set_reg(sel.kernel_data);
        load_tss(sel.tss);
    }

    // set up the IDT
    let idt = create_idt();
    let idt = Box::leak(Box::new(idt));
    idt.load();

    let lapic = lapic::init();

    // create the execution context for the CPU and store it
    {
        let ctx = ExecutionContext::new(cpu, gdt, sel, idt, tss, lapic);
        let addr = VirtAddr::from_ptr(Box::leak(Box::new(ctx)));
        KernelGsBase::write(addr);
    }

    sse::init();

    init_interrupts();

    // load it back and print a message
    let ctx = ExecutionContext::load();
    info!("cpu {} initialized", ctx.cpu_id());

    interrupts::enable();
}

unsafe extern "C" fn cpu_init_and_idle(cpu: &limine::mp::Cpu) -> ! {
    unsafe { cpu_init_and_return(cpu) };

    turn_idle()
}

/// Parks the current task as this core's idle task, never returning.
///
/// The task is not pinned to this core, so the scheduler may hand it to any core
/// that runs out of ordinary work.
pub fn turn_idle() -> ! {
    // Must precede the first halt. While this task sits in the ordinary queue it
    // costs every runnable task a timer quantum each time it is picked.
    ExecutionContext::load().current_task().mark_idle();
    loop {
        hlt();
    }
}

fn init_interrupts() {
    let mut io_apic = io_apic().lock();
    unsafe {
        const OFFSET: u8 = 32;
        io_apic.init(OFFSET);

        // TODO: redirect interrupt vectors

        // for vector in 0..u8::MAX - OFFSET {
        //     let mut entry = RedirectionTableEntry::default();
        //     entry.set_mode(IrqMode::Fixed);
        //     entry.set_flags(IrqFlags::LEVEL_TRIGGERED | IrqFlags::LOW_ACTIVE);
        //     entry.set_vector(vector);
        //     entry.set_dest(u8::try_from(lapic_id).expect("invalid lapic id"));
        //
        //     io_apic.set_table_entry(vector, entry);
        //     io_apic.enable_irq(vector);
        // }
    }
}
