#![no_std]

use core::arch::asm;
use core::arch::x86_64::_mm_pause;
use core::ffi::c_int;

pub fn exit() -> ! {
    syscall0(1);
    loop {
        unsafe {
            _mm_pause();
        }
    }
}

pub fn read(fd: c_int, buf: &mut [u8]) -> c_int {
    syscall3(36, fd as usize, buf.as_mut_ptr() as usize, buf.len()) as i32
}

pub fn write(fd: c_int, buf: &[u8]) -> c_int {
    syscall3(37, fd as usize, buf.as_ptr() as usize, buf.len()) as i32
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
