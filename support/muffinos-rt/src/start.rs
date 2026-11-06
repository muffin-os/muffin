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
    // The callee saves r12, so the entry `rsp` stays in r12 during `init_main_thread`.
    "mov r12, rsp",
    "mov rdi, [r12]",
    // envp starts after the argc argv pointers and their NULL.
    "lea rax, [r12 + rdi * 8 + 16]",
    "mov [rip + {environ}], rax",
    "and rsp, -16",
    "call {init_main_thread}",
    "mov rdi, [r12]",
    "lea rsi, [r12 + 8]",
    "call {main}",
    "mov edi, eax",
    "call {exit}",
    environ = sym crate::env::environ,
    init_main_thread = sym crate::thread::init_main_thread,
    main = sym main,
    exit = sym crate::process::exit,
);
