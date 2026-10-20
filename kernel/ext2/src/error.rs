use core::fmt::{Debug, Display, Formatter};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Error {
    InvalidSuperblock,
    UnableToReadSuperblock,
    UnableToWriteSuperblock,
    UnableToReadBlockGroupDescriptorTable,
    UnableToWriteBlockGroupDescriptorTable,
    InvalidBlockGroupDescriptor(u32),
    NotDirectory,
    NotRegularFile,
    DeviceRead,
    DeviceWrite,
    InvalidInodeAddress(u32),
    InvalidBlockIndex(u32),
    InvalidDirEntryType(u8),
    NoSpace,
    NotSupported,
    EntryExists,
    NameTooLong,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Debug::fmt(&self, f)
    }
}

impl core::error::Error for Error {}
