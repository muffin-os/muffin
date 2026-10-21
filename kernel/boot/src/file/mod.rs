use core::ops::Deref;
use core::sync::atomic::{AtomicU64, Ordering};

use kernel_syscall::access::AccessMode;
use kernel_vfs::Vfs;
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::AbsolutePath;
use spin::RwLock;
use tracing::{Level, instrument};

use crate::file::devfs::devfs;

pub mod devfs;
pub mod ext2;

static VFS: RwLock<Vfs> = RwLock::new(Vfs::new());

#[must_use]
pub fn vfs() -> &'static RwLock<Vfs> {
    &VFS
}

#[instrument(name = "init filesystem", level = Level::DEBUG)]
pub fn init() {
    devfs::init();

    VFS.write()
        .mount(AbsolutePath::try_new("/dev").unwrap(), devfs().clone())
        .expect("should be able to mount devfs");
}

#[derive(Debug)]
pub struct OpenFileDescription {
    position: AtomicU64,
    node: VfsNode,
    access: AccessMode,
    /// Every write starts at the current end of the file.
    append: bool,
}

impl Clone for OpenFileDescription {
    fn clone(&self) -> Self {
        let position = self.position.load(Ordering::Relaxed);
        Self {
            position: AtomicU64::new(position),
            node: self.node.clone(),
            access: self.access,
            append: self.append,
        }
    }
}

impl Deref for OpenFileDescription {
    type Target = VfsNode;

    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

impl OpenFileDescription {
    #[must_use]
    pub fn new(node: VfsNode, access: AccessMode, append: bool) -> Self {
        Self {
            position: AtomicU64::new(0),
            node,
            access,
            append,
        }
    }

    pub fn position(&self) -> &AtomicU64 {
        &self.position
    }

    #[must_use]
    pub fn access(&self) -> AccessMode {
        self.access
    }

    #[must_use]
    pub fn is_append(&self) -> bool {
        self.append
    }
}
