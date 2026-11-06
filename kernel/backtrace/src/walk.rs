use core::arch::asm;

/// Yields the return addresses of the frame-pointer chain, starting with the
/// return address into the caller of [`with_return_addresses`].
pub struct ReturnAddresses {
    current_bp: *const usize,
}

/// Calls `f` with a walk of the current call stack.
///
/// The walk reads the stack frames above this function, so it is only valid
/// while this function's own frame is live. That is why the walk is passed to
/// a closure and not returned.
#[inline(never)]
pub fn with_return_addresses<R>(f: impl FnOnce(ReturnAddresses) -> R) -> R {
    let current_bp: *const usize;
    unsafe {
        asm!(
        "mov {bp}, rbp",
        bp = out(reg) current_bp,
        options(nomem, nostack, preserves_flags),
        );
    }
    f(ReturnAddresses { current_bp })
}

impl Iterator for ReturnAddresses {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let current_bp = self.current_bp;
        // The frame below the syscall entry holds the user rbp, and the user
        // half may already be unmapped, so the walk ends at the kernel half.
        if (current_bp as isize) >= 0 || !current_bp.is_aligned() {
            return None;
        }

        let next_bp = unsafe { *current_bp };
        let instruction_pointer = unsafe { *(current_bp.add(1)) };

        self.current_bp = next_bp as *const usize;
        if instruction_pointer == 0 {
            None
        } else {
            Some(instruction_pointer)
        }
    }
}
