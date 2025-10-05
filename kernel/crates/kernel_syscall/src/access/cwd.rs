use kernel_vfs::path::AbsoluteOwnedPath;
use spin::RwLock;

pub trait CwdAccess {
    fn current_working_directory(&self) -> &RwLock<AbsoluteOwnedPath>;
}
