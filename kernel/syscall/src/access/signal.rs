use kernel_abi::{ProcessId, SigInfo};

pub trait SignalAccess {
    fn deliver(&self, pid: ProcessId, info: SigInfo);
}
