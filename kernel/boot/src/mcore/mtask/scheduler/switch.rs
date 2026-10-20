use core::arch::naked_asm;

macro_rules! push_context {
    () => {
        concat!(
            r#"
			pushfq
			push rax
			push rbx
			push rcx
			push rdx
			sub  rsp, 8
            push rbp
			push rsi
			push rdi
			push r8
			push r9
			push r10
			push r11
			push r12
			push r13
			push r14
			push r15
			"#,
        )
    };
}

macro_rules! pop_context {
    () => {
        concat!(
            r#"
			pop r15
			pop r14
			pop r13
			pop r12
			pop r11
			pop r10
			pop r9
			pop r8
			pop rdi
			pop rsi
			pop rbp
			add rsp, 8
			pop rdx
			pop rcx
			pop rbx
			pop rax
			popfq
			"#
        )
    };
}

macro_rules! set_task_switched {
    () => {
        concat!(
            r#"
            mov rax, cr0
            or rax, 8
            mov cr0, rax
			"#
        )
    };
}

/// Saves the current context, writes the resulting stack pointer to
/// `_old_stack`, loads `_new_cr3_value` into CR3 and resumes the context
/// saved at `_new_stack`. Returns only when something later switches back
/// to the saved context, with interrupts still disabled.
///
/// # Safety
/// Interrupts must be disabled. `_old_stack` must be valid for a write of
/// one `usize`. `_new_stack` must point at a context pushed by this function
/// or laid out like one, on a stack that is mapped in the address space
/// `_new_cr3_value` selects, and `_new_cr3_value` must be a valid CR3 value.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_impl(
    _old_stack: *mut usize,
    _new_stack: *const u8,
    _new_cr3_value: usize,
) {
    // SysV ABI: rdi = _old_stack, rsi = _new_stack, rdx = _new_cr3_value.
    naked_asm!(
        push_context!(),
        "mov [rdi], rsp",
        "mov rsp, rsi",
        set_task_switched!(),
        "mov cr3, rdx",
        pop_context!(),
        "ret"
    )
}
