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

/// Perform a context switch.
///
/// `_old_stack` is the pointer where the current stack pointer will be written to.
/// ```notrust
/// *_old_stack = $rsp;
/// ```
/// `_new_stack` is the stack pointer that we want to switch to.
/// ```notrust
/// $rsp = _new_stack;
/// ```
///
/// Notice that `_old_stack` is being dereferenced, while `_new_stack` is not.
///
/// # Safety
///
/// Disable interrupts before you call this. Interrupts remain disabled across
/// this call; the caller is responsible for re-enabling.
///
/// Switching to another context is unsafe, as it executes
/// some other code without any drop or safety guarantees
/// about the caller of this method. The caller must ensure
/// that `_old_stack` and `_new_stack` are valid pointers to
/// a thread stack.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_impl(
    _old_stack: *mut usize,
    _new_stack: *const u8,
    _new_cr3_value: usize,
) {
    // _old_stack is located in $rdi, _new_stack is in $rsi
    // $rdi -> old_stack
    // $rsi -> new_stack
    // $rdx -> new_cr3_value

    naked_asm!(
        push_context!(),
        "mov [rdi], rsp", // write the stack pointer rsp at *_old_stack
        "mov rsp, rsi",   // write _new_stack into rsp
        set_task_switched!(),
        "mov cr3, rdx", // write _new_cr3_value into cr3
        pop_context!(),
        "ret"
    )
}
