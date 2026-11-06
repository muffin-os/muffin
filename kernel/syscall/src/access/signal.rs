use kernel_abi::{ProcessId, SigInfo};

use crate::signal::SignalMask;

pub trait SignalAccess {
    fn deliver(&self, pid: ProcessId, info: SigInfo);
}

/// This trait gives access to the signal mask of the calling thread.
pub trait SignalMaskAccess {
    fn signal_mask(&self) -> SignalMask;
    fn set_signal_mask(&self, mask: SignalMask);
}
