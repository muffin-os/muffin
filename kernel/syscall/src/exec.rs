use core::mem::size_of;

const WORD: usize = size_of::<usize>();
const STACK_ALIGN: usize = 16;

/// Writes the SysV x86-64 process-entry stack into `stack`, whose last byte
/// is mapped at virtual address `stack_top - 1`. Returns the virtual address
/// the new image starts with in rsp. The result points at argc and is 16 byte
/// aligned. None when the payload does not fit into `stack`.
pub fn build_initial_stack(
    stack: &mut [u8],
    stack_top: usize,
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Option<usize> {
    let mut strings_len = 0usize;
    for s in argv.iter().chain(envp.iter()) {
        strings_len = strings_len.checked_add(s.len())?.checked_add(1)?;
    }

    let words = argv.len().checked_add(envp.len())?.checked_add(3)?;
    let pointers_len = words.checked_mul(WORD)?;

    let base = stack_top.checked_sub(stack.len())?;
    let rsp = stack_top
        .checked_sub(strings_len)?
        .checked_sub(pointers_len)?
        & !(STACK_ALIGN - 1);
    if rsp < base {
        return None;
    }

    let mut string_addr = stack_top - strings_len;
    let mut word_addr = rsp;

    put_word(stack, base, word_addr, argv.len())?;
    word_addr += WORD;

    for group in [argv, envp] {
        for s in group {
            put_word(stack, base, word_addr, string_addr)?;
            word_addr += WORD;

            let at = string_addr - base;
            stack.get_mut(at..at + s.len())?.copy_from_slice(s);
            *stack.get_mut(at + s.len())? = 0;
            string_addr += s.len() + 1;
        }

        put_word(stack, base, word_addr, 0)?;
        word_addr += WORD;
    }

    Some(rsp)
}

fn put_word(stack: &mut [u8], base: usize, addr: usize, value: usize) -> Option<()> {
    let at = addr.checked_sub(base)?;
    stack
        .get_mut(at..at.checked_add(WORD)?)?
        .copy_from_slice(&value.to_le_bytes());
    Some(())
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const STACK_TOP: usize = 0x1_0000;

    fn read_word(stack: &[u8], stack_top: usize, addr: usize) -> usize {
        let base = stack_top - stack.len();
        let at = addr - base;
        let mut bytes = [0u8; WORD];
        bytes.copy_from_slice(&stack[at..at + WORD]);
        usize::from_le_bytes(bytes)
    }

    fn string_at(stack: &[u8], stack_top: usize, addr: usize) -> &[u8] {
        let base = stack_top - stack.len();
        let at = addr - base;
        let end = at
            + stack[at..]
                .iter()
                .position(|&b| b == 0)
                .expect("no NUL terminator");
        &stack[at..end]
    }

    #[test]
    fn layout() {
        let argv: [&[u8]; 3] = [b"exec-target", b"alpha", b"beta"];
        let envp: [&[u8]; 2] = [b"KEY=value", b"HOME=/"];
        let mut stack = vec![0xAAu8; 512];

        let rsp = build_initial_stack(&mut stack, STACK_TOP, &argv, &envp)
            .expect("payload fits into 512 bytes");

        assert_eq!(rsp % STACK_ALIGN, 0, "rsp {rsp:#x} is not 16 byte aligned");
        assert_eq!(
            read_word(&stack, STACK_TOP, rsp),
            argv.len(),
            "argc word mismatch"
        );

        let mut addr = rsp + WORD;
        for (i, expected) in argv.iter().enumerate() {
            let ptr = read_word(&stack, STACK_TOP, addr);
            assert_ne!(ptr, 0, "argv[{i}] pointer is NULL");
            assert_eq!(
                string_at(&stack, STACK_TOP, ptr),
                *expected,
                "argv[{i}] content mismatch"
            );
            addr += WORD;
        }
        assert_eq!(
            read_word(&stack, STACK_TOP, addr),
            0,
            "argv array is not NULL terminated"
        );
        addr += WORD;

        for (i, expected) in envp.iter().enumerate() {
            let ptr = read_word(&stack, STACK_TOP, addr);
            assert_ne!(ptr, 0, "envp[{i}] pointer is NULL");
            assert_eq!(
                string_at(&stack, STACK_TOP, ptr),
                *expected,
                "envp[{i}] content mismatch"
            );
            addr += WORD;
        }
        assert_eq!(
            read_word(&stack, STACK_TOP, addr),
            0,
            "envp array is not NULL terminated"
        );
    }

    #[test]
    fn empty_argv_and_envp() {
        let mut stack = vec![0xAAu8; 64];

        let rsp = build_initial_stack(&mut stack, STACK_TOP, &[], &[])
            .expect("empty payload always fits");

        assert_eq!(rsp % STACK_ALIGN, 0, "rsp {rsp:#x} is not 16 byte aligned");
        assert_eq!(read_word(&stack, STACK_TOP, rsp), 0, "argc must be zero");
        assert_eq!(
            read_word(&stack, STACK_TOP, rsp + WORD),
            0,
            "argv array is not NULL terminated"
        );
        assert_eq!(
            read_word(&stack, STACK_TOP, rsp + 2 * WORD),
            0,
            "envp array is not NULL terminated"
        );
    }

    #[test]
    fn exact_fit_and_one_byte_short() {
        let argv: [&[u8]; 2] = [b"exec-target", b"alpha"];
        let envp: [&[u8]; 1] = [b"KEY=value"];
        let strings = 12 + 6 + 10;
        let words = argv.len() + envp.len() + 3;
        let needed = (strings + words * WORD).next_multiple_of(STACK_ALIGN);

        let mut exact = vec![0u8; needed];
        let rsp = build_initial_stack(&mut exact, STACK_TOP, &argv, &envp)
            .expect("exact fit buffer must succeed");
        assert_eq!(
            rsp,
            STACK_TOP - needed,
            "rsp must land on the first byte of an exactly sized buffer"
        );

        let mut short = vec![0u8; needed - 1];
        assert_eq!(
            build_initial_stack(&mut short, STACK_TOP, &argv, &envp),
            None,
            "a buffer one byte too small must be rejected"
        );
    }
}
