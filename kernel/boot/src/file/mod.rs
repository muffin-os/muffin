use alloc::sync::Arc;
use core::ops::Deref;
use core::sync::atomic::AtomicU64;

use kernel_syscall::access::AccessMode;
use kernel_vfs::Vfs;
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::AbsolutePath;
use spin::RwLock;
use tracing::{Level, instrument};

use crate::file::devfs::devfs;
use crate::file::shm::ShmFs;
use crate::file::socket::Socket;

pub mod devfs;
pub mod ext2;
pub mod shm;
pub mod socket;

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
    VFS.write()
        .mount(AbsolutePath::try_new("/dev/shm").unwrap(), ShmFs::default())
        .expect("should be able to mount shmfs");
}

#[derive(Debug)]
pub struct OpenFile {
    position: AtomicU64,
    node: VfsNode,
    access: AccessMode,
    /// Every write starts at the current end of the file.
    append: bool,
}

#[derive(Debug)]
pub enum OpenFileDescription {
    File(OpenFile),
    Socket(Arc<Socket>),
}

impl OpenFileDescription {
    #[must_use]
    pub fn file(&self) -> Option<&OpenFile> {
        match self {
            Self::File(file) => Some(file),
            Self::Socket(_) => None,
        }
    }

    #[must_use]
    pub fn socket(&self) -> Option<&Arc<Socket>> {
        match self {
            Self::File(_) => None,
            Self::Socket(socket) => Some(socket),
        }
    }
}

impl Deref for OpenFile {
    type Target = VfsNode;

    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

impl OpenFile {
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
