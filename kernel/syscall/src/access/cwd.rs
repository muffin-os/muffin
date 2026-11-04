use alloc::borrow::ToOwned;

use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, Path};
use spin::RwLock;

pub trait CwdAccess {
    fn current_working_directory(&self) -> &RwLock<AbsoluteOwnedPath>;

    /// Joins a relative `path` to the working directory.
    fn resolve(&self, path: &str) -> AbsoluteOwnedPath {
        let path = Path::new(path);
        if let Ok(p) = AbsolutePath::try_new(path) {
            p.to_owned()
        } else {
            let mut p = self.current_working_directory().read().clone();
            p.push(path);
            p
        }
    }
}
