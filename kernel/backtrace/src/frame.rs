use core::fmt::{Display, Formatter};

use crate::ksym::{KernelSymbols, Symbol};

/// One return address of a backtrace and the function that contains it.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Frame<'a> {
    pub instruction_pointer: usize,
    pub symbol: Option<Symbol<'a>>,
}

impl Display for Frame<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let ip = self.instruction_pointer;
        match self.symbol {
            Some(Symbol { name, offset }) => write!(f, "{ip:#018x} @ {name}+{offset:#x}"),
            None => write!(f, "{ip:#018x} @ <unknown>"),
        }
    }
}

/// Symbolizes return addresses one at a time, so a backtrace needs no heap.
pub struct Backtrace<'a, I> {
    addresses: I,
    symbols: Option<&'a KernelSymbols<'a>>,
}

impl<'a, I: Iterator<Item = usize>> Backtrace<'a, I> {
    /// Without `symbols`, every frame has no symbol but keeps its address.
    pub fn new(addresses: I, symbols: Option<&'a KernelSymbols<'a>>) -> Self {
        Self { addresses, symbols }
    }
}

impl<'a, I: Iterator<Item = usize>> Iterator for Backtrace<'a, I> {
    type Item = Frame<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let ip = self.addresses.next()?;
        // A return address points past the call instruction, which can be the
        // first byte of the next function. One byte back is inside the caller.
        let symbol = self
            .symbols
            .and_then(|s| s.resolve(ip.saturating_sub(1) as u64));
        Some(Frame {
            instruction_pointer: ip,
            symbol,
        })
    }
}
