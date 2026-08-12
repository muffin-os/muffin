use core::arch::asm;
use core::arch::x86_64::_fxrstor;
use core::fmt::{Debug, Formatter};
use core::mem::{offset_of, transmute};
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::Signal;
use tracing::{error, warn};
use x86_64::instructions::{hlt, interrupts};
use x86_64::registers::control::Cr2;
use x86_64::registers::debug::{Dr6, Dr7};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::structures::paging::{Page, Size4KiB};
use x86_64::{PrivilegeLevel, VirtAddr};

use crate::U64Ext;
use crate::arch::{gdt, signal};
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::mem::{MemoryRegion, PageInError};
use crate::mcore::mtask::task::Task;
use crate::mcore::mtask::wait::wake_expired_sleepers;
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
            .set_handler_fn(transmute::<
                *mut fn(),
                extern "x86-interrupt" fn(InterruptStackFrame, PageFaultErrorCode),
            >(page_fault_wrapper as *mut fn()))
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

    unsafe {
        idt[InterruptIndex::Timer.as_u8()].set_handler_fn(transmute::<
            *mut fn(),
            extern "x86-interrupt" fn(InterruptStackFrame),
        >(
            timer_interrupt_handler as *mut fn()
        ));
    }
    idt[InterruptIndex::LapicErr.as_u8()].set_handler_fn(lapic_err_interrupt_handler);
    idt[InterruptIndex::Spurious.as_u8()].set_handler_fn(spurious_interrupt_handler);

    unsafe {
        idt[InterruptIndex::Syscall.as_u8()]
            .set_handler_fn(transmute::<
                *mut fn(),
                extern "x86-interrupt" fn(InterruptStackFrame),
            >(syscall_handler as *mut fn()))
            .set_privilege_level(PrivilegeLevel::Ring3)
            .disable_interrupts(false);
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
wrap!(timer_interrupt_handler_impl => timer_interrupt_handler);

#[repr(align(8), C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SyscallRegisters {
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
    stack_frame: &mut InterruptStackFrame,
    regs: &mut SyscallRegisters,
) {
    if regs.rax == kernel_abi::SYS_SIGRETURN {
        signal::sys_sigreturn(stack_frame, regs);
        return;
    }

    let result = dispatch_syscall(stack_frame, regs);

    regs.rax = result as usize;
}

pub extern "sysv64" fn timer_interrupt_handler_impl(
    stack_frame: &mut InterruptStackFrame,
    regs: &mut SyscallRegisters,
) {
    unsafe {
        end_of_interrupt();
    }

    wake_expired_sleepers();

    // only deliver signals when we're in userspace
    if stack_frame.code_segment.rpl() == PrivilegeLevel::Ring3 {
        signal::deliver_pending(stack_frame, regs);
    }

    unsafe {
        ExecutionContext::load().scheduler_mut().reschedule();
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

#[repr(C)]
pub(crate) struct CalleeSavedRegisters {
    pub rbx: usize,
    pub rbp: usize,
    pub r12: usize,
    pub r13: usize,
    pub r14: usize,
    pub r15: usize,
}

#[repr(C)]
pub(crate) struct FaultBlock {
    pub regs: SyscallRegisters,
    pub callee: CalleeSavedRegisters,
    pub error_code: u64,
    pub frame: InterruptStackFrame,
}

const _: () = {
    assert!(168 == size_of::<FaultBlock>());
    assert!(0 == offset_of!(FaultBlock, regs));
    assert!(120 == offset_of!(FaultBlock, error_code));
    assert!(128 == offset_of!(FaultBlock, frame));
};

#[allow(clippy::missing_safety_doc)]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn page_fault_wrapper() {
    core::arch::naked_asm!(
        "push r15",
        "push r14",
        "push r13",
        "push r12",
        "push rbp",
        "push rbx",
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "sub rsp, 8",
        "lea rdi, [rsp + 136]",
        "lea rsi, [rsp + 8]",
        "mov rdx, [rsp + 128]",
        "call {classify}",
        "add rsp, 8",
        "test rax, rax",
        "jz 2f",
        "mov rdi, rax",
        "sub rdi, 168",
        "mov rsi, rsp",
        "mov rcx, 21",
        "cld",
        "rep movsq",
        "mov rsp, rax",
        "sub rsp, 168",
        "mov rdi, rsp",
        "sub rsp, 8",
        "sti",
        "call {pager}",
        "cli",
        "add rsp, 8",
        "2:",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "pop rbx",
        "pop rbp",
        "pop r12",
        "pop r13",
        "pop r14",
        "pop r15",
        "add rsp, 8",
        "iretq",
        classify = sym page_fault_classify,
        pager = sym page_fault_pager,
    );
}

fn pager_stack_top(task: &Task, frame: &InterruptStackFrame, from_user: bool) -> Option<usize> {
    let stack = task.kstack().as_ref()?;
    if from_user {
        return Some(stack.top().as_u64().into_usize());
    }

    let mapped = stack.mapped_segment();
    let rsp = frame.stack_pointer;
    if rsp <= mapped.start || rsp > mapped.start + mapped.len {
        return None;
    }
    Some((rsp.as_u64().into_usize() - 128) & !0xf)
}

fn terminate_faulting_task(
    frame: &mut InterruptStackFrame,
    regs: &mut SyscallRegisters,
    task: &Task,
) {
    if frame.code_segment.rpl() == PrivilegeLevel::Ring3 {
        signal::deliver_fault(frame, regs, Signal::Segfault);
        return;
    }

    // Returning would retry the faulting instruction and fault forever, and a
    // Ring 0 fault has no user context to redirect. Only the scheduler can reap
    // the task, so interrupts have to come back on before halting.
    task.set_should_terminate(true);
    interrupts::enable();
    loop {
        hlt();
    }
}

extern "sysv64" fn page_fault_classify(
    frame: &mut InterruptStackFrame,
    regs: &mut SyscallRegisters,
    error_code: u64,
) -> usize {
    let error_code = PageFaultErrorCode::from_bits_truncate(error_code);
    let accessed_address = Cr2::read().ok();
    let from_user = frame.code_segment.rpl() == PrivilegeLevel::Ring3;

    // if we know the address...
    if let Some(addr) = accessed_address
        && let Some(ctx) = ExecutionContext::try_load()
    {
        let task = ctx.current_task();
        let process = task.process();
        process.telemetry().page_faults.fetch_add(1, Relaxed);

        // ...and the current task has stack, then the accessed address must not be within the
        // guard page of the stack, otherwise we have a stack overflow...
        if let Some(stack) = task.kstack()
            && stack.guard_page().contains(addr)
        {
            error!(
                "KERNEL STACK OVERFLOW DETECTED in process '{}' task '{}', terminating...",
                process.name(),
                task.name(),
            );
            terminate_faulting_task(frame, regs, task);
            return 0;
        }

        // ...but if it's not a stack issue, maybe it is a lazy mapping?
        if let Some(region) = process.memory_regions().region_for(addr) {
            let reason = if error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
                "protection violation"
            } else {
                match &*region {
                    MemoryRegion::Lazy(_) | MemoryRegion::FileBacked(_) => {
                        match pager_stack_top(task, frame, from_user) {
                            Some(top) => {
                                task.pending_fault_addr().store(addr.as_u64(), Relaxed);
                                return top;
                            }
                            None => "demand paging fault on an unusable stack",
                        }
                    }
                    MemoryRegion::Mapped(_) => "invalid access to a mapped region",
                    // A shared device mapping is fully mapped eagerly, so a
                    // fault inside it is an invalid access.
                    MemoryRegion::Shared(_) => "invalid access to a shared region",
                }
            };

            error!(
                reason,
                "page fault at {addr:p} in process '{}' task '{}', terminating...",
                process.name(),
                task.name()
            );
            terminate_faulting_task(frame, regs, task);
            return 0;
        }
    }

    // A wild user pointer must become a SIGSEGV, never a kernel panic.
    if from_user {
        error!("unhandled user page fault at {accessed_address:?}, delivering SIGSEGV");
        signal::deliver_fault(frame, regs, Signal::Segfault);
        return 0;
    }

    panic!(
        "EXCEPTION: PAGE FAULT:\naccessed address: {accessed_address:?}\nerror code: {error_code:#?}\n{frame:#?}"
    );
}

extern "sysv64" fn page_fault_pager(block: *mut FaultBlock) {
    let ctx = ExecutionContext::load();
    let task = ctx.current_task();
    let process = task.process();
    let addr = VirtAddr::new(task.pending_fault_addr().swap(0, Relaxed));
    let page = Page::<Size4KiB>::containing_address(addr);
    let address_space = process.address_space();

    let region = process.memory_regions().region_for(addr);
    let failure = match region.as_deref() {
        Some(MemoryRegion::Lazy(r)) => r.map_zeroed(address_space, page).err(),
        Some(MemoryRegion::FileBacked(r)) => r.page_in(address_space, page).err(),
        _ => Some(PageInError::MapFailed),
    };

    if let Some(e) = failure {
        error!(
            error = %e,
            "failed to page in {addr:p} in process '{}' task '{}', terminating...",
            process.name(),
            task.name()
        );
        let block = unsafe { &mut *block };
        terminate_faulting_task(&mut block.frame, &mut block.regs, task);
    }
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
    let guard = current_task.fx_area().read();
    let fx_area_ptr = guard.as_ref().map(|fx| fx.start().as_mut_ptr::<u8>());
    drop(guard); // _fxrstor could trigger #NM again, so we must drop the guard before calling it

    unsafe { asm!("clts") };

    if let Some(ptr) = fx_area_ptr {
        unsafe { _fxrstor(ptr) };
    }
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
