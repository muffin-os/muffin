use conquer_once::spin::OnceCell;
use kernel_backtrace::{Backtrace, KernelSymbols, ReturnAddresses};
use tracing::{Level, instrument};

static KERNEL_SYMBOLS: OnceCell<KernelSymbols<'static>> = OnceCell::uninit();

/// Loads the kernel symbols from the `.ksym` Limine module. Without the
/// module, backtraces show raw addresses.
#[instrument(name = "init backtrace context", level = Level::DEBUG)]
pub fn init() {
    #[cfg(feature = "backtrace")]
    {
        use core::slice::from_raw_parts;

        use tracing::{debug, warn};

        use crate::U64Ext;
        use crate::limine::MODULE_REQUEST;

        let Some(module) = MODULE_REQUEST.get_response().and_then(|response| {
            response
                .modules()
                .iter()
                .find(|m| m.path().to_bytes().ends_with(b".ksym"))
        }) else {
            warn!("no kernel symbol module, backtraces will show raw addresses");
            return;
        };
        let data = unsafe {
            // Safety: we keep the part of limine's higher half mapping that contains
            // the modules, so dereferencing that pointer is safe.
            from_raw_parts(module.addr().cast_const(), module.size().into_usize())
        };
        match KernelSymbols::parse(data) {
            Ok(symbols) => {
                debug!(
                    symbol_count = symbols.symbol_count(),
                    "kernel symbols loaded"
                );
                KERNEL_SYMBOLS.init_once(|| symbols);
            }
            Err(e) => warn!(error = %e, "invalid kernel symbol module"),
        }
    }
}

/// Calls `f` with the symbolized frames of the current call stack, innermost
/// first. The frames need no heap, so a panic handler can use them.
pub fn with_backtrace<R>(f: impl FnOnce(Backtrace<'static, ReturnAddresses>) -> R) -> R {
    kernel_backtrace::with_return_addresses(|addresses| {
        f(Backtrace::new(addresses, KERNEL_SYMBOLS.get()))
    })
}
