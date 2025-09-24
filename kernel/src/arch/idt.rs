use core::alloc::Layout;
use core::arch::asm;
use core::arch::x86_64::{_fxrstor, _fxsave};
use core::fmt::{Debug, Formatter};
use core::mem::transmute;

use kernel_memapi::{Guarded, Location, MemoryApi, UserAccessible};
use log::{error, warn};
use x86_64::PrivilegeLevel;
use x86_64::instructions::{hlt, interrupts};
use x86_64::registers::control::Cr2;
use x86_64::registers::debug::{Dr6, Dr7};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::arch::gdt;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::task::FxArea;
use crate::mem::memapi::LowerHalfMemoryApi;
use crate::syscall::dispatch_syscall;

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    /// 32
    Timer = 0x20,
    /// 49
    LapicErr = 0x31,
    Syscall = 0x80,
    /// 255
    Spurious = 0xff,
}

impl InterruptIndex {
    pub fn as_usize(self) -> usize {
        self as usize
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

pub fn create_idt() -> InterruptDescriptorTable {
    let mut idt = InterruptDescriptorTable::new();

    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        idt.page_fault
            .set_handler_fn(page_fault_handler)
            .set_stack_index(gdt::PAGE_FAULT_IST_INDEX);
    }

    idt.debug.set_handler_fn(debug_handler);
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.device_not_available
        .set_handler_fn(device_not_available_handler);

    idt.general_protection_fault
        .set_handler_fn(general_protection_fault_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
    idt.invalid_tss.set_handler_fn(invalid_tss_handler);
    idt.segment_not_present
        .set_handler_fn(segment_not_present_handler);
    idt.stack_segment_fault
        .set_handler_fn(stack_segment_fault_handler);

    idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
    idt[InterruptIndex::LapicErr.as_u8()].set_handler_fn(lapic_err_interrupt_handler);
    idt[InterruptIndex::Spurious.as_u8()].set_handler_fn(spurious_interrupt_handler);

    unsafe {
        idt[InterruptIndex::Syscall.as_u8()]
            .set_handler_fn(transmute::<
                *mut fn(),
                extern "x86-interrupt" fn(InterruptStackFrame),
            >(syscall_handler as *mut fn()))
            .set_privilege_level(PrivilegeLevel::Ring3)
            .disable_interrupts(true);
    }

    idt
}

macro_rules! wrap {
    ($fn:ident => $w:ident) => {
        #[allow(clippy::missing_safety_doc)]
        #[unsafe(naked)]
        pub unsafe extern "sysv64" fn $w() {
            core::arch::naked_asm!(
                "push rax",
                "push rcx",
                "push rdx",
                "push rsi",
                "push rdi",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "mov rsi, rsp", // Arg #2: register list
                "mov rdi, rsp", // Arg #1: interupt frame
                "add rdi, 9 * 8",
                "call {}",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rdi",
                "pop rsi",
                "pop rdx",
                "pop rcx",
                "pop rax",
                "iretq",
                sym $fn
            );
        }
    };
}

wrap!(syscall_handler_impl => syscall_handler);

#[repr(align(8), C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SyscallRegisters {
    pub r11: usize,
    pub r10: usize,
    pub r9: usize,
    pub r8: usize,
    pub rdi: usize,
    pub rsi: usize,
    pub rdx: usize,
    pub rcx: usize,
    pub rax: usize,
}

pub extern "sysv64" fn syscall_handler_impl(
    _stack_frame: &mut InterruptStackFrame,
    regs: &mut SyscallRegisters,
) {
    // The registers order follow the System V ABI convention
    let n = regs.rax;
    let arg1 = regs.rdi;
    let arg2 = regs.rsi;
    let arg3 = regs.rdx;
    let arg4 = regs.rcx;
    let arg5 = regs.r8;
    let arg6 = regs.r9;

    let result = dispatch_syscall(n, arg1, arg2, arg3, arg4, arg5, arg6);

    regs.rax = result as usize; // save result
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    unsafe {
        end_of_interrupt();
    }

    let ctx = ExecutionContext::load();
    unsafe {
        ctx.scheduler_mut().reschedule();
    }
}

extern "x86-interrupt" fn lapic_err_interrupt_handler(stack_frame: InterruptStackFrame) {
    panic!("EXCEPTION: LAPIC ERROR\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn spurious_interrupt_handler(stack_frame: InterruptStackFrame) {
    panic!("EXCEPTION: SPURIOUS INTERRUPT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(stack_frame: InterruptStackFrame, _: u64) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT:\n{stack_frame:#?}");
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    panic!(
        "EXCEPTION: GENERAL PROTECTION FAULT:\nerror code: {error_code:#X}\n{}[{}], external: {}\n{stack_frame:#?}",
        match (error_code >> 1) & 0b11 {
            0 => "GDT",
            2 => "LDT",
            _ => "IDT",
        },
        (error_code >> 3) & ((1 << 14) - 1),
        (error_code & 1) > 0
    );
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    panic!("EXCEPTION: INVALID OPCODE:\n{stack_frame:#?}");
}

extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    panic!("EXCEPTION: INVALID TSS:\nerror code: {error_code:#X}\n{stack_frame:#?}");
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let accessed_address = Cr2::read().ok();

    // if we know the address...
    if let Some(addr) = accessed_address {
        // ...and we have initialized multitasking...
        if let Some(ctx) = ExecutionContext::try_load() {
            let task = ctx.current_task();
            // ...and the current task has stack...
            if let Some(stack) = task.kstack() {
                // ...then the accessed address must not be within the guard page of the stack,
                // otherwise we have a stack overflow...
                if stack.guard_page().contains(addr) {
                    error!(
                        "KERNEL STACK OVERFLOW DETECTED in process '{}' task '{}', terminating...",
                        task.process().name(),
                        task.name(),
                    );

                    // ...in which case we mark the task for termination...
                    task.set_should_terminate(true);
                    // ...and halt, waiting for the scheduler to terminate the task
                    interrupts::enable();
                    loop {
                        hlt();
                    }
                }
            }
        }
    }

    panic!(
        "EXCEPTION: PAGE FAULT:\naccessed address: {accessed_address:?}\nerror code: {error_code:#?}\n{stack_frame:#?}"
    );
}

extern "x86-interrupt" fn segment_not_present_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    let error_code = SelectorErrorCode::from(error_code);
    panic!("EXCEPTION: SEGMENT NOT PRESENT:\nerror code: {error_code:#?}\n{stack_frame:#?}");
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    panic!("EXCEPTION: STACK SEGMENT FAULT:\nerror code: {error_code:#?}\n{stack_frame:#?}");
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    warn!("BREAKPOINT:\n{stack_frame:#?}");
    warn!("halting...");
    loop {
        hlt();
    }
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    warn!("DEBUG:\n{stack_frame:#?}");
    let dr6_flags = Dr6::read();
    warn!("DR6 flags: {dr6_flags:#?}");
    let dr7_flags = Dr7::read();
    warn!("DR7 flags: {dr7_flags:#?}");
}

extern "x86-interrupt" fn device_not_available_handler(_stack_frame: InterruptStackFrame) {
    let cx = ExecutionContext::load();
    let current_task = cx.current_task();

    let mut guard = current_task.fx_area().write();
    let (fresh, fx_area) = if let Some(fx_area) = &*guard {
        (false, fx_area)
    } else {
        let process = current_task.process();
        let mut memapi = LowerHalfMemoryApi::new(process.clone());
        let fx_area = memapi
            .allocate(
                Location::Anywhere,
                Layout::new::<FxArea>(),
                UserAccessible::Yes,
                Guarded::No,
            )
            .expect("should be able to allocate fx area");

        (true, guard.insert(fx_area) as &_)
    };

    let fx_area_ptr = fx_area.start().as_mut_ptr::<u8>();
    drop(guard); // _fxrstor could trigger #NM again, so we must drop the guard before calling it

    unsafe { asm!("clts") };

    // saving is done every time we switch tasks, so we can only restore it here
    if fresh {
        unsafe {
            asm!("finit");
            _fxsave(fx_area_ptr);
        }
    }
    unsafe { _fxrstor(fx_area_ptr) };
}

/// Notifies the LAPIC that the interrupt has been handled.
///
/// # Safety
/// This is unsafe since it writes to an LAPIC register.
#[inline]
pub unsafe fn end_of_interrupt() {
    let ctx = ExecutionContext::load();
    unsafe { ctx.lapic().lock().end_of_interrupt() };
}

#[repr(transparent)]
struct SelectorErrorCode(u32);

impl From<u32> for SelectorErrorCode {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<u64> for SelectorErrorCode {
    fn from(value: u64) -> Self {
        let value = u32::try_from(value).unwrap();
        value.into()
    }
}

impl SelectorErrorCode {
    fn external(&self) -> bool {
        (self.0 & 1) > 0
    }

    fn tbl(&self) -> u8 {
        ((self.0 >> 1) & 0b11) as u8
    }

    fn index(&self) -> u16 {
        ((self.0 >> 3) & ((1 << 14) - 1)) as u16
    }
}

impl Debug for SelectorErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SelectorErrorCode")
            .field("index", &self.index())
            .field(
                "tbl",
                &match self.tbl() {
                    0b00 => "GDT",
                    0b01 | 0b11 => "IDT",
                    0b10 => "LDT",
                    _ => unreachable!(),
                },
            )
            .field("external", &self.external())
            .finish()
    }
}
