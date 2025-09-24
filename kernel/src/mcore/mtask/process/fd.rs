use alloc::sync::Arc;
use core::cmp::Ordering;
use core::ffi::c_int;
use core::fmt::Debug;

use bitflags::bitflags;
use spin::rwlock::RwLock;

use crate::file::OpenFileDescription;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct FdNum(i32);

impl FdNum {
    pub const STDIN: Self = Self(0);
    pub const STDOUT: Self = Self(1);
    pub const STDERR: Self = Self(2);
}

impl From<c_int> for FdNum {
    fn from(value: c_int) -> Self {
        Self(value)
    }
}

impl From<FdNum> for c_int {
    fn from(value: FdNum) -> Self {
        value.0
    }
}

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
