use kernel_abi::{Errno, ProcessId};

mod cwd;
mod file;
mod mem;
mod process;
mod region;
mod signal;

pub use cwd::*;
pub use file::*;
pub use mem::*;
pub use process::*;
pub use region::*;
pub use signal::*;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Capability {
    Signal,
    Debug,
    Priority,
}

pub trait PermissionAccess {
    fn current_identity(&self) -> Identity;
    fn check_permission(&self, target_pid: ProcessId, cap: Capability) -> Result<(), Errno>;
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Identity {
    pub process_id: ProcessId,
    pub user_id: u32,
    pub process_group_id: ProcessId,
}
