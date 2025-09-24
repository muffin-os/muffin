#![no_std]
#![no_main]

use core::arch::asm;
use core::ffi::CStr;

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    let bytes = b"hello from init!\n\0";
    let text = unsafe { CStr::from_bytes_with_nul_unchecked(bytes) };
    // write
    syscall3(37, 2, text.as_ptr() as usize, bytes.len() - 1);
    // exit
    syscall0(1);
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

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &::core::panic::PanicInfo) -> ! {
    loop {}
}
