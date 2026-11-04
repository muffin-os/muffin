//! Process entry. The kernel enters `_start` with `rsp` at `argc`, followed
//! by the argv pointer array, a NULL, the envp pointer array and a NULL.

use core::ffi::{c_char, c_int};

unsafe extern "C" {
    /// The C `main` that rustc emits for a std binary.
    fn main(argc: c_int, argv: *const *const c_char) -> c_int;
}

core::arch::global_asm!(
    ".globl _start",
    "_start:",
    "mov rdi, [rsp]",
    "lea rsi, [rsp + 8]",
    // envp starts after the argc argv pointers and their NULL.
    "lea rax, [rsi + rdi * 8 + 8]",
    "mov [rip + {environ}], rax",
    "and rsp, -16",
    "call {main}",
    "mov edi, eax",
    "call {exit}",
    environ = sym crate::env::environ,
    main = sym main,
    exit = sym crate::process::exit,
);
