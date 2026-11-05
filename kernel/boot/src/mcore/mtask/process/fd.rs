use alloc::sync::Arc;
use core::cmp::Ordering;
use core::fmt::Debug;

use bitflags::bitflags;
use kernel_abi::FdNum;
use spin::rwlock::RwLock;

use crate::file::OpenFileDescription;

#[derive(Debug)]
pub struct FileDescriptor {
    num: FdNum,

    _flags: RwLock<FileDescriptorFlags>,
    file_description: Arc<OpenFileDescription>,
}

impl PartialEq for FileDescriptor {
    fn eq(&self, other: &Self) -> bool {
        self.num == other.num
    }
}

impl Eq for FileDescriptor {}

impl PartialOrd for FileDescriptor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FileDescriptor {
    fn cmp(&self, other: &Self) -> Ordering {
        self.num.cmp(&other.num)
    }
}

impl FileDescriptor {
    pub fn new(
        num: FdNum,
        flags: FileDescriptorFlags,
        file_description: Arc<OpenFileDescription>,
    ) -> Self {
        Self {
            num,
            _flags: RwLock::new(flags),
            file_description,
        }
    }

    pub fn num(&self) -> FdNum {
        self.num
    }

    pub fn flags(&self) -> FileDescriptorFlags {
        *self._flags.read()
    }

    pub fn file_description(&self) -> &Arc<OpenFileDescription> {
        &self.file_description
    }
}

bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct FileDescriptorFlags: u32 {
        const READABLE = 0b00000001;
    }
}
