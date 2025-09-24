use core::ops::Deref;
use core::sync::atomic::{AtomicU64, Ordering};

use kernel_vfs::Vfs;
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::AbsolutePath;
use spin::RwLock;

use crate::file::devfs::devfs;

pub mod devfs;
pub mod ext2;

static VFS: RwLock<Vfs> = RwLock::new(Vfs::new());

#[must_use]
pub fn vfs() -> &'static RwLock<Vfs> {
    &VFS
}

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
}

impl From<VfsNode> for OpenFileDescription {
    fn from(node: VfsNode) -> Self {
        Self {
            position: AtomicU64::new(0),
            node,
        }
    }
}

impl Clone for OpenFileDescription {
    fn clone(&self) -> Self {
        let position = self.position.load(Ordering::Relaxed);
        Self {
            position: AtomicU64::new(position),
            node: self.node.clone(),
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
    pub fn position(&self) -> &AtomicU64 {
        &self.position
    }
}
