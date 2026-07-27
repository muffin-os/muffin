#![no_std]

use core::arch::asm;
use core::arch::x86_64::_mm_pause;
use core::ffi::c_int;

pub use kernel_abi::{DefaultAction, SaFlags, SigAction, SigHandler, SigMaskHow, SigSet, Signal};

pub fn exit(code: i32) -> ! {
    syscall1(1, code as usize);
    loop {
        _mm_pause();
    }
}

pub fn read(fd: c_int, buf: &mut [u8]) -> c_int {
    syscall3(36, fd as usize, buf.as_mut_ptr() as usize, buf.len()) as i32
}

pub fn write(fd: c_int, buf: &[u8]) -> c_int {
    syscall3(37, fd as usize, buf.as_ptr() as usize, buf.len()) as i32
}

#[unsafe(naked)]
pub extern "C" fn sigreturn_restorer() {
    core::arch::naked_asm!("mov rax, 46", "int 0x80")
}

pub fn kill(pid: i64, signo: Signal) -> c_int {
    syscall2(42, pid as usize, signo.number() as usize) as c_int
}

pub fn sigaction(signo: Signal, new: Option<&SigAction>, old: Option<&mut SigAction>) -> c_int {
    let new_ptr = new.map_or(0, |a| a as *const SigAction as usize);
    let old_ptr = old.map_or(0, |a| a as *mut SigAction as usize);
    syscall3(43, signo.number() as usize, new_ptr, old_ptr) as c_int
}

pub fn sigprocmask(how: SigMaskHow, set: Option<&SigSet>, old: Option<&mut SigSet>) -> c_int {
    let set_ptr = set.map_or(0, |s| s as *const SigSet as usize);
    let old_ptr = old.map_or(0, |s| s as *mut SigSet as usize);
    syscall3(44, how as usize, set_ptr, old_ptr) as c_int
}

pub fn sigpending(out: &mut SigSet) -> c_int {
    syscall1(45, out as *mut SigSet as usize) as c_int
}

pub fn getpid() -> i64 {
    syscall0(47) as i64
}

pub fn install_handler(signo: Signal, handler: extern "C" fn(Signal)) -> c_int {
    let action = SigAction {
        handler: SigHandler::new(handler as usize),
        mask: 0,
        flags: SaFlags::default(),
        restorer: sigreturn_restorer as *const () as usize,
    };
    sigaction(signo, Some(&action), None)
}

pub fn syscall0(n: usize) -> usize {
    let mut result;
    unsafe {
        asm!(
        "mov rax, {n}",
        "int 0x80",
        "mov {result}, rax",
        n = in(reg) n,
        result = lateout(reg) result,
        );
    }
    result
}

pub fn syscall1(n: usize, arg1: usize) -> usize {
    let mut result;
    unsafe {
        asm!(
        "mov rax,{n}",
        "mov rdi, {arg1}",
        "int 0x80",
        "mov {result}, rax",
        n = in(reg) n,
        arg1 = in(reg) arg1,
        result = lateout(reg) result,
        );
    }
    result
}

pub fn syscall2(n: usize, arg1: usize, arg2: usize) -> usize {
    let mut result;
    unsafe {
        asm!(
        "mov rax,{n}",
        "mov rdi, {arg1}",
        "mov rsi, {arg2}",
        "int 0x80",
        "mov {result}, rax",
        n = in(reg) n,
        arg1 = in(reg) arg1,
        arg2 = in(reg) arg2,
        result = lateout(reg) result,
        );
    }
    result
}

pub fn syscall3(n: usize, arg1: usize, arg2: usize, arg3: usize) -> usize {
    let mut result;
    unsafe {
        asm!(
        "mov rax,{n}",
        "mov rdi, {arg1}",
        "mov rsi, {arg2}",
        "mov rdx, {arg3}",
        "int 0x80",
        "mov {result}, rax",
        n = in(reg) n,
        arg1 = in(reg) arg1,
        arg2 = in(reg) arg2,
        arg3 = in(reg) arg3,
        result = lateout(reg) result,
        );
    }
    result
}
