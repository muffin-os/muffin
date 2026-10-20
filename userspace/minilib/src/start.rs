//! Process entry. The kernel enters `_start` with `rsp` pointing at `argc`,
//! 16 byte aligned, followed by the argv pointer array, a NULL, the envp
//! pointer array, a NULL, and the NUL terminated string bytes above.

use core::ptr::null_mut;
use core::slice;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::exit;

/// Defines the process entry point. The binary supplies `fn main() -> i32`,
/// whose return value becomes the exit code.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(naked)]
        #[unsafe(no_mangle)]
        pub extern "C" fn _start() -> ! {
            core::arch::naked_asm!(
                "mov rdi, rsp",
                "and rsp, -16",
                "call {start}",
                start = sym $crate::__muffin_start_inner,
            )
        }

        #[unsafe(no_mangle)]
        extern "C" fn __muffin_main() -> i32 {
            $main()
        }
    };
}

static ARGV: AtomicPtr<*const u8> = AtomicPtr::new(null_mut());
static ARGC: AtomicUsize = AtomicUsize::new(0);
static ENVP: AtomicPtr<*const u8> = AtomicPtr::new(null_mut());
static ENVC: AtomicUsize = AtomicUsize::new(0);

/// # Safety
///
/// `sp` must be the stack pointer the kernel entered the image with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __muffin_start_inner(sp: *const usize) -> ! {
    unsafe { rt_init(sp) };

    unsafe extern "C" {
        fn __muffin_main() -> i32;
    }

    let code = unsafe { __muffin_main() };
    exit(code)
}

/// # Safety
///
/// `sp` must point at the `argc` word of the process entry stack.
unsafe fn rt_init(sp: *const usize) {
    let argc = unsafe { *sp };
    let argv = unsafe { sp.add(1) }.cast_mut().cast::<*const u8>();
    let envp = unsafe { argv.add(argc + 1) };

    let mut envc = 0;
    while !unsafe { *envp.add(envc) }.is_null() {
        envc += 1;
    }

    ARGV.store(argv, Ordering::Relaxed);
    ARGC.store(argc, Ordering::Relaxed);
    ENVP.store(envp, Ordering::Relaxed);
    ENVC.store(envc, Ordering::Relaxed);
}

/// Arguments the image was executed with, NUL excluded.
pub fn args() -> impl Iterator<Item = &'static [u8]> {
    strings(&ARGV, &ARGC)
}

/// Environment of the image, one `KEY=value` entry per item, NUL excluded.
pub fn env() -> impl Iterator<Item = &'static [u8]> {
    strings(&ENVP, &ENVC)
}

fn strings(
    base: &AtomicPtr<*const u8>,
    count: &AtomicUsize,
) -> impl Iterator<Item = &'static [u8]> {
    let base = base.load(Ordering::Relaxed);
    // Safety: the strings live on the initial stack, which is never reclaimed, so
    // they outlive every caller.
    (0..count.load(Ordering::Relaxed)).map(move |i| unsafe { bytes_of(*base.add(i)) })
}

/// # Safety
///
/// `ptr` must point at a NUL terminated string that lives for the process.
unsafe fn bytes_of(ptr: *const u8) -> &'static [u8] {
    let mut len = 0;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    unsafe { slice::from_raw_parts(ptr, len) }
}
