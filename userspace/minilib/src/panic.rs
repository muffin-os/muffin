//! Panic runtime for muffin userspace.
//!
//! The `panic_impl` lang item lives here, so every userspace binary reports on
//! stderr, unwinds running destructors, and exits 101 when nothing catches.

use alloc::boxed::Box;
use alloc::string::ToString;
use core::any::Any;
use core::fmt::Write;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use unwinding::custom_eh_frame_finder::{
    EhFrameFinder, FrameInfo, FrameInfoKind, set_custom_eh_frame_finder,
};

const E_PHOFF: usize = 0x20;
const E_PHENTSIZE: usize = 0x36;
const E_PHNUM: usize = 0x38;
const P_VADDR: usize = 0x10;
const PT_GNU_EH_FRAME: u32 = 0x6474_E550;

unsafe extern "C" {
    /// Load address of the process's own ELF header, resolved by the linker.
    static __ehdr_start: u8;
}

/// Answers the unwinder's FDE lookups from the process's own program headers.
///
/// Yields `None` for a binary without `PT_GNU_EH_FRAME`, which surfaces as an
/// escaped panic rather than a corrupt walk.
struct PhdrFinder;

unsafe impl EhFrameFinder for PhdrFinder {
    fn find(&self, _pc: usize) -> Option<FrameInfo> {
        // SAFETY: the loader maps every `PT_LOAD` of a static `ET_EXEC` and the
        // first spans file offset 0, so the ELF header and the program header
        // table it declares stay readable at `__ehdr_start` for the process
        // lifetime. The reads stay inside them. The offsets are ELF64 constants,
        // and reading unaligned assumes nothing about the mapping's layout.
        unsafe {
            let base = &raw const __ehdr_start;
            let e_phoff = base.add(E_PHOFF).cast::<u64>().read_unaligned() as usize;
            let e_phentsize = base.add(E_PHENTSIZE).cast::<u16>().read_unaligned() as usize;
            let e_phnum = base.add(E_PHNUM).cast::<u16>().read_unaligned() as usize;
            (0..e_phnum)
                .map(|i| base.add(e_phoff + i * e_phentsize))
                .find(|ph| ph.cast::<u32>().read_unaligned() == PT_GNU_EH_FRAME)
                .map(|ph| FrameInfo {
                    text_base: Some(base as usize),
                    kind: FrameInfoKind::EhFrameHdr(
                        ph.add(P_VADDR).cast::<u64>().read_unaligned() as usize
                    ),
                })
        }
    }
}

static FINDER: PhdrFinder = PhdrFinder;

// Depth of the panic currently being raised, zero while no panic is in flight.
// Process global, which matches per-task depth only because userspace has no
// threads.
static PANIC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Runs `f` and catches a panic raised anywhere inside it.
///
/// The `Err` payload holds the formatted message as an [`alloc::string::String`],
/// matching std, so a caller recovers it with `downcast_ref::<String>()`.
///
/// This is the only sound catch path. Catching through the unwinder directly
/// makes the next panic exit without a report.
pub fn catch_unwind<R>(f: impl FnOnce() -> R) -> Result<R, Box<dyn Any + Send>> {
    let result = unwinding::panic::catch_unwind(f);
    if result.is_err() {
        // Disarms the nested-panic guard in `panic`.
        PANIC_COUNT.store(0, Ordering::Relaxed);
    }
    result
}

/// Reports the panic on stderr with a backtrace, then unwinds so destructors run.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Raising an exception allocates, and so does symbolizing the backtrace. A
    // heap that cannot serve either panics again, and this bounds that recursion
    // at one level, which also covers a panic escaping a `Drop` mid-unwind. Those
    // processes end without completing destructors, with the report already out.
    if PANIC_COUNT.fetch_add(1, Ordering::Relaxed) > 0 {
        crate::exit(101);
    }
    let _ = writeln!(crate::Stderr, "{info}");
    // There is no init hook, so the first panic installs the finder. It stays for
    // the process lifetime and the repeat call reports exactly that. The walk
    // below needs the finder, and must finish before `begin_panic` unwinds the
    // frames it walks.
    let _ = set_custom_eh_frame_finder(&FINDER);
    crate::backtrace::print(&mut crate::Stderr);
    unwinding::panic::begin_panic(Box::new(info.message().to_string()));
    // Reached only when nothing caught the exception or an FDE was missing. 101
    // is std's exit code for a panic.
    crate::exit(101)
}
