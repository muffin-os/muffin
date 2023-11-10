use x86_64::structures::idt::InterruptStackFrame;

use crate::arch::idt;
use crate::syscall::dispatch_syscall;

#[repr(align(8), C)]
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::arch) struct SyscallRegisters {
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

pub(in crate::arch) extern "sysv64" fn syscall_handler_impl(
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

    let res = dispatch_syscall(n, arg1, arg2, arg3, arg4, arg5, arg6);

    regs.rax = res as usize; // save result

    unsafe {
        idt::end_of_interrupt();
    }
}
