use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};

use kernel_vfs::OpenError;

use crate::DevFile;

pub struct DevNode {
    name: String,
    kind: DevNodeKind,
}

impl DevNode {
    pub fn new(name: String, kind: DevNodeKind) -> Self {
        Self { name, kind }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Deref for DevNode {
    type Target = DevNodeKind;

    fn deref(&self) -> &Self::Target {
        &self.kind
    }
}

impl DerefMut for DevNode {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.kind
    }
}

pub enum DevNodeKind {
    Directory(DevDirectoryNode),
    File(DevFileNode),
}

impl DevNodeKind {
    pub fn directory(&self) -> Option<&DevDirectoryNode> {
        if let DevNodeKind::Directory(dir) = self {
            Some(dir)
        } else {
            None
        }
    }

    pub fn directory_mut(&mut self) -> Option<&mut DevDirectoryNode> {
        if let DevNodeKind::Directory(dir) = self {
            Some(dir)
        } else {
            None
        }
    }

    pub fn file(&self) -> Option<&DevFileNode> {
        if let DevNodeKind::File(file) = self {
            Some(file)
        } else {
            None
        }
    }
}

pub struct DevDirectoryNode {
    children: Vec<DevNode>,
}

impl From<Vec<DevNode>> for DevDirectoryNode {
    fn from(value: Vec<DevNode>) -> Self {
        Self { children: value }
    }
}

impl DevDirectoryNode {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn lookup_child(&self, name: &str) -> Option<&DevNode> {
        self.children.iter().find(|node| node.name() == name)
    }

    pub fn lookup_child_mut(&mut self, name: &str) -> Option<&mut DevNode> {
        self.children.iter_mut().find(|node| node.name() == name)
    }

    pub fn children_mut(&mut self) -> &mut Vec<DevNode> {
        &mut self.children
    }
}

pub struct DevFileNode {
    open_fn: Box<dyn Fn() -> Result<Box<dyn DevFile>, OpenError> + Send + Sync>,
}

impl DevFileNode {
    pub fn new<F>(open_fn: F) -> Self
    where
        F: Fn() -> Result<Box<dyn DevFile>, OpenError> + Send + Sync + 'static,
    {
        Self {
            open_fn: Box::new(open_fn),
        }
    }

    pub fn open_fn(&self) -> &(dyn Fn() -> Result<Box<dyn DevFile>, OpenError> + Send + Sync) {
        &self.open_fn
    }
}
