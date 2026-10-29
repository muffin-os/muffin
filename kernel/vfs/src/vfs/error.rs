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
    #[error("open failed")]
    OpenFailed,
    #[error("already exists")]
    AlreadyExists,
    #[error("no space left to create the file")]
    NoSpace,
    #[error("the file name is longer than the file system allows")]
    NameTooLong,
    #[error("the file system cannot create files")]
    CreateUnsupported,
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
    #[error("read failed")]
    ReadFailed,
    #[error("file is not readable")]
    NotReadable,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum ReadDirError {
    #[error("{0}")]
    FsError(
        #[from]
        #[source]
        FsError,
    ),
    #[error("file is not a directory")]
    NotDirectory,
    #[error("the file system cannot list directories")]
    NotSupported,
    #[error("read dir failed")]
    ReadFailed,
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
    #[error("no space left for the written bytes")]
    NoSpace,
    #[error("the write would end past the largest size the file system supports")]
    FileTooLarge,
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

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum TruncateError {
    #[error("{0}")]
    FsError(
        #[from]
        #[source]
        FsError,
    ),
    #[error("file cannot be truncated")]
    NotSupported,
    #[error("truncate failed")]
    TruncateFailed,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum MkdirError {
    #[error("the parent directory does not exist")]
    NotFound,
    #[error("already exists")]
    AlreadyExists,
    #[error("no space left to create the directory")]
    NoSpace,
    #[error("the directory name is longer than the file system allows")]
    NameTooLong,
    #[error("the file system cannot create directories")]
    NotSupported,
    #[error("mkdir failed")]
    MkdirFailed,
}
