use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::IoctlRequest;
use kernel_vfs::fs::{FileSystem, FsHandle};
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{
    CloseError, CreateMode, FsError, FsyncError, IoctlError, MmapError, MmapRegion, OpenError,
    ReadError, Stat, StatError, WriteError,
};
use thiserror::Error;

use crate::node::{DevDirectoryNode, DevFileNode, DevNode, DevNodeKind};
use crate::{DevFile, Null, Zero};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum RegisterError {
    #[error("resolve: {0}")]
    ResolveError(#[from] ResolveError),
    #[error("the file at the specified path already exists")]
    AlreadyExists,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum ResolveError {
    #[error("the parent of the path does not exist")]
    ParentNotFound,
    #[error("the path is not a directory")]
    ParentNotDirectory,
}

impl From<ResolveError> for OpenError {
    fn from(_: ResolveError) -> Self {
        OpenError::NotFound
    }
}

pub struct DevFs {
    root: DevNode,
    open_files: BTreeMap<FsHandle, Box<dyn DevFile>>,
}

impl Default for DevFs {
    fn default() -> Self {
        Self::new()
    }
}

impl DevFs {
    pub fn new() -> Self {
        let mut v = Self {
            root: DevNode::new(
                String::from("/"),
                DevNodeKind::Directory(DevDirectoryNode::new()),
            ),
            open_files: BTreeMap::new(),
        };

        fn setup(v: &mut DevFs) -> Result<(), RegisterError> {
            v.register_file(AbsolutePath::try_new("/null").unwrap(), || Ok(Null))?;
            v.register_file(AbsolutePath::try_new("/zero").unwrap(), || Ok(Zero))?;
            Ok(())
        }
        setup(&mut v).expect("should be able to register default files");

        v
    }

    pub fn register_file<O, F>(
        &mut self,
        path: &AbsolutePath,
        open_fn: O,
    ) -> Result<(), RegisterError>
    where
        O: Fn() -> Result<F, OpenError> + Send + Sync + 'static,
        F: DevFile + 'static,
    {
        let parent = path.parent().ok_or(ResolveError::ParentNotFound)?;
        let filename = path.file_name().ok_or(ResolveError::ParentNotFound)?;

        let parent_node = self.resolve_node_mut(parent)?;
        let parent_dir = parent_node
            .directory_mut()
            .ok_or(RegisterError::ResolveError(
                ResolveError::ParentNotDirectory,
            ))?;
        if parent_dir.lookup_child(filename).is_some() {
            return Err(RegisterError::AlreadyExists);
        }

        let file_node = DevNode::new(
            filename.to_string(),
            DevNodeKind::File(DevFileNode::new(Box::new(move || {
                open_fn().map(|file| Box::new(file) as Box<dyn DevFile>)
            }))),
        );
        parent_dir.children_mut().push(file_node);
        Ok(())
    }

    fn resolve_node(&self, path: &AbsolutePath) -> Result<&DevNode, ResolveError> {
        let mut current_node = &self.root;
        for component in path.filenames() {
            let dir_node = current_node
                .directory()
                .ok_or(ResolveError::ParentNotDirectory)?;
            if let Some(child) = dir_node.lookup_child(component) {
                current_node = child;
            } else {
                return Err(ResolveError::ParentNotFound);
            }
        }

        Ok(current_node)
    }

    fn resolve_node_mut(&mut self, path: &AbsolutePath) -> Result<&mut DevNode, ResolveError> {
        let mut current_node = &mut self.root;
        for component in path.filenames() {
            let dir_node = current_node
                .directory_mut()
                .ok_or(ResolveError::ParentNotDirectory)?;
            if let Some(child) = dir_node.lookup_child_mut(component) {
                current_node = child;
            } else {
                return Err(ResolveError::ParentNotFound);
            }
        }

        Ok(current_node)
    }

    fn new_fs_handle() -> FsHandle {
        static FS_COUNTER: AtomicU64 = AtomicU64::new(0);
        FsHandle::from(FS_COUNTER.fetch_add(1, Relaxed))
    }

    fn resolve_handle(&mut self, handle: FsHandle) -> Result<&mut Box<dyn DevFile>, FsError> {
        self.open_files
            .get_mut(&handle)
            .ok_or(FsError::InvalidHandle)
    }
}

impl FileSystem for DevFs {
    fn open(&mut self, path: &AbsolutePath, create: CreateMode) -> Result<FsHandle, OpenError> {
        let node = match (self.resolve_node(path), create) {
            (Ok(_), CreateMode::Exclusive) => return Err(OpenError::AlreadyExists),
            (Ok(node), CreateMode::Never | CreateMode::IfMissing) => node,
            (Err(e), CreateMode::Never) => return Err(e.into()),
            (Err(e), CreateMode::IfMissing | CreateMode::Exclusive) => {
                let parent_is_directory = path
                    .parent()
                    .and_then(|parent| self.resolve_node(parent).ok())
                    .is_some_and(|parent| parent.directory().is_some());
                return Err(if parent_is_directory {
                    OpenError::CreateUnsupported
                } else {
                    e.into()
                });
            }
        };
        let file_node = node.file().ok_or(OpenError::NotFound)?;
        let file = file_node.open_fn()()?;
        let handle = Self::new_fs_handle();
        self.open_files.insert(handle, file);
        Ok(handle)
    }

    fn close(&mut self, handle: FsHandle) -> Result<(), CloseError> {
        self.open_files.remove(&handle).ok_or(CloseError::NotOpen)?;
        Ok(())
    }

    fn read(
        &mut self,
        handle: FsHandle,
        buf: &mut [u8],
        offset: usize,
    ) -> Result<usize, ReadError> {
        self.resolve_handle(handle)?.read(buf, offset)
    }

    fn write(&mut self, handle: FsHandle, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        self.resolve_handle(handle)?.write(buf, offset)
    }

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError> {
        self.resolve_handle(handle)?.stat(stat)
    }

    fn mmap(&mut self, handle: FsHandle) -> Result<MmapRegion, MmapError> {
        self.resolve_handle(handle)
            .map_err(MmapError::FsError)?
            .mmap()
    }

    fn ioctl(
        &mut self,
        handle: FsHandle,
        request: IoctlRequest,
        arg: &mut [u8],
    ) -> Result<usize, IoctlError> {
        self.resolve_handle(handle)
            .map_err(IoctlError::FsError)?
            .ioctl(request, arg)
    }

    fn fsync(&mut self, handle: FsHandle) -> Result<(), FsyncError> {
        self.resolve_handle(handle)
            .map_err(FsyncError::FsError)?
            .fsync()
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::sync::atomic::AtomicUsize;
    use core::sync::atomic::Ordering::{Acquire, Release};

    use kernel_vfs::path::ROOT;

    use super::*;

    #[test]
    fn test_open_not_found() {
        let mut devfs = DevFs::new();
        let path = AbsolutePath::try_new("/nonexistent").unwrap();
        let result = devfs.open(path, CreateMode::Never);
        assert_eq!(result, Err(OpenError::NotFound));
    }

    #[test]
    fn test_open_directory_is_not_found() {
        let mut devfs = DevFs::new();
        assert_eq!(
            devfs.open(ROOT, CreateMode::Never),
            Err(OpenError::NotFound)
        );
    }

    #[test]
    fn create_on_missing_path_is_unsupported() {
        let mut devfs = DevFs::new();
        let path = AbsolutePath::try_new("/newfile").unwrap();

        for create in [CreateMode::IfMissing, CreateMode::Exclusive] {
            assert_eq!(
                Err(OpenError::CreateUnsupported),
                devfs.open(path, create),
                "{create:?}"
            );
        }
        assert_eq!(
            Err(OpenError::NotFound),
            devfs.open(path, CreateMode::Never),
            "a rejected create must not leave a file behind"
        );
    }

    #[test]
    fn create_in_missing_directory_is_not_found() {
        let mut devfs = DevFs::new();
        let path = AbsolutePath::try_new("/nodir/newfile").unwrap();

        assert_eq!(
            Err(OpenError::NotFound),
            devfs.open(path, CreateMode::IfMissing)
        );
    }

    #[test]
    fn create_on_existing_file_opens_it_unless_exclusive() {
        let mut devfs = DevFs::new();
        let null = AbsolutePath::try_new("/null").unwrap();

        let handle = devfs
            .open(null, CreateMode::IfMissing)
            .expect("IfMissing must open the existing /null");
        assert_eq!(Ok(4), devfs.write(handle, b"data", 0));
        assert_eq!(
            Err(OpenError::AlreadyExists),
            devfs.open(null, CreateMode::Exclusive)
        );
    }

    #[derive(Debug, Eq, PartialEq)]
    struct TestDevFile {
        id: usize,
        data: Vec<u8>,
    }

    impl TestDevFile {
        pub fn new() -> Self {
            static ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
            TestDevFile {
                id: ID_COUNTER.fetch_add(1, Relaxed),
                data: Vec::new(),
            }
        }
    }

    impl DevFile for TestDevFile {
        fn read(&mut self, buf: &mut [u8], offset: usize) -> Result<usize, ReadError> {
            let offset = offset.min(self.data.len());
            let len = buf.len().min(self.data.len() - offset);
            buf[..len].copy_from_slice(&self.data[offset..offset + len]);

            Ok(len)
        }

        fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
            let end = offset + buf.len();
            if end > self.data.len() {
                self.data.resize(end, 0);
            }
            self.data[offset..end].copy_from_slice(buf);
            Ok(buf.len())
        }

        fn stat(&mut self, _stat: &mut Stat) -> Result<(), StatError> {
            Ok(())
        }
    }

    #[test]
    fn test_register_root() {
        let mut devfs = DevFs::new();

        // Registering root should not fail
        let err = devfs
            .register_file(ROOT, || Ok(TestDevFile::new()))
            .unwrap_err();
        assert_eq!(
            err,
            RegisterError::ResolveError(ResolveError::ParentNotFound)
        );
    }

    #[test]
    fn test_register_open_close() {
        let path = AbsolutePath::try_new("/testfile").unwrap();

        let mut devfs = DevFs::new();
        devfs
            .register_file(path, || Ok(TestDevFile::new()))
            .expect("should be able to register file");

        let file = devfs
            .open(path, CreateMode::Never)
            .expect("should be able to open registered file");

        assert_eq!(Ok(()), devfs.close(file));
        assert_eq!(
            Err(ReadError::FsError(FsError::InvalidHandle)),
            devfs.read(file, &mut [0; 1], 0)
        );
        assert_eq!(Err(CloseError::NotOpen), devfs.close(file));
    }

    #[test]
    fn test_open_multiple() {
        let path = AbsolutePath::try_new("/testfile").unwrap();

        let open_counter = Arc::new(AtomicUsize::new(0));

        let mut devfs = DevFs::new();
        devfs
            .register_file(path, {
                let open_counter = open_counter.clone();
                move || {
                    open_counter.fetch_add(1, Release);
                    Ok(TestDevFile::new())
                }
            })
            .expect("should be able to register file");

        let file1 = devfs
            .open(path, CreateMode::Never)
            .expect("should be able to open registered file");
        let file2 = devfs
            .open(path, CreateMode::Never)
            .expect("should be able to open registered file");

        assert_ne!(
            file1, file2,
            "should be able to open multiple instances of the same file"
        );
        assert_eq!(2, open_counter.load(Acquire), "open counter should be 2");
    }

    #[test]
    fn test_write_read() {
        let path = AbsolutePath::try_new("/testfile").unwrap();

        let mut devfs = DevFs::new();
        devfs
            .register_file(path, || Ok(TestDevFile::new()))
            .expect("should be able to register file");

        let file = devfs
            .open(path, CreateMode::Never)
            .expect("should be able to open registered file");

        let write_buf = b"hello";
        let bytes_written = devfs
            .write(file, write_buf, 0)
            .expect("should be able to write to file");
        assert_eq!(bytes_written, write_buf.len(), "should write all bytes");

        let mut read_buf = vec![0; write_buf.len()];
        let bytes_read = devfs
            .read(file, &mut read_buf, 0)
            .expect("should be able to read from file");
        assert_eq!(bytes_read, write_buf.len(), "should read all bytes");

        assert_eq!(read_buf, write_buf, "read buffer should match written data");
    }

    #[test]
    fn test_write_read_at_offset() {
        let path = AbsolutePath::try_new("/testfile").unwrap();

        let mut devfs = DevFs::new();
        devfs
            .register_file(path, || Ok(TestDevFile::new()))
            .expect("should be able to register file");
        let file = devfs.open(path, CreateMode::Never).unwrap();

        assert_eq!(Ok(5), devfs.write(file, b"hello", 0));
        assert_eq!(Ok(2), devfs.write(file, b"LL", 2));

        let mut read_buf = [0; 2];
        assert_eq!(Ok(2), devfs.read(file, &mut read_buf, 3));
        assert_eq!(b"Lo", &read_buf);
    }
}
