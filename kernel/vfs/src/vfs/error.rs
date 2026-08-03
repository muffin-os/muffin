use thiserror::Error;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum MountError {
    #[error("the mount point is already used by another mount")]
    AlreadyMounted,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum UnmountError {
    #[error("not mounted")]
    NotMounted,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum ExistsError {}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum OpenError {
    #[error("not found")]
    NotFound,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum CloseError {
    #[error("not open")]
    NotOpen,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum FsError {
    #[error("filesystem is not open")]
    FileSystemNotOpen,
    #[error("invalid handle")]
    InvalidHandle,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum ReadError {
    #[error("{0}")]
    FsError(
        #[from]
        #[source]
        FsError,
    ),
    #[error("end of file")]
    EndOfFile,
    #[error("read failed")]
    ReadFailed,
    #[error("file is not readable")]
    NotReadable,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum WriteError {
    #[error("{0}")]
    FsError(
        #[from]
        #[source]
        FsError,
    ),
    #[error("write failed")]
    WriteFailed,
    #[error("file is not writable")]
    NotWritable,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum StatError {
    #[error("{0}")]
    FsError(
        #[from]
        #[source]
        FsError,
    ),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum IoctlError {
    #[error("{0}")]
    FsError(
        #[from]
        #[source]
        FsError,
    ),
    #[error("file does not support this ioctl request")]
    NotSupported,
    #[error("invalid ioctl argument")]
    InvalidArgument,
}
