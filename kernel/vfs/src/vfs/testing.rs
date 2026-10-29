use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::iter::successors;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

use spin::RwLock;

use crate::fs::{FileSystem, FsHandle};
use crate::path::{AbsoluteOwnedPath, AbsolutePath, ROOT};
use crate::{
    CloseError, CreateMode, FsError, FsyncError, MkdirError, OpenError, ReadDirError, ReadError,
    Stat, StatError, TruncateError, WriteError,
};

#[derive(Default)]
pub struct TestFs {
    handle_counter: AtomicU64,
    files: BTreeMap<AbsoluteOwnedPath, RwLock<Vec<u8>>>,
    directories: BTreeSet<AbsoluteOwnedPath>,
    open_files: BTreeMap<FsHandle, AbsoluteOwnedPath>,
}

impl TestFs {
    pub fn insert_file(&mut self, path: impl AsRef<AbsolutePath>, data: Vec<u8>) {
        self.files
            .insert(path.as_ref().to_owned(), RwLock::new(data));
    }

    // Besides the created ones, the root and every ancestor of a file are directories.
    fn has_directory(&self, dir: &AbsolutePath) -> bool {
        dir == ROOT
            || self.directories.contains(dir)
            || self
                .files
                .keys()
                .any(|file| successors(file.as_ref().parent(), |p| p.parent()).any(|p| p == dir))
    }
}

impl FileSystem for TestFs {
    fn open(&mut self, path: &AbsolutePath, create: CreateMode) -> Result<FsHandle, OpenError> {
        // shortcut: read, write, truncate and stat panic on a directory handle;
        // return errors once a test needs them.
        let exists = self.files.contains_key(path) || self.has_directory(path);
        match (exists, create) {
            (true, CreateMode::Exclusive) => return Err(OpenError::AlreadyExists),
            (true, CreateMode::Never | CreateMode::IfMissing) => {}
            (false, CreateMode::Never) => return Err(OpenError::NotFound),
            (false, CreateMode::IfMissing | CreateMode::Exclusive) => {
                if !path
                    .parent()
                    .is_some_and(|parent| self.has_directory(parent))
                {
                    return Err(OpenError::NotFound);
                }
                self.insert_file(path, Vec::new());
            }
        }
        let handle = FsHandle::from(self.handle_counter.fetch_add(1, Relaxed));
        self.open_files.insert(handle, path.to_owned());
        Ok(handle)
    }

    fn create_directory(&mut self, path: &AbsolutePath) -> Result<(), MkdirError> {
        if self.files.contains_key(path) || self.has_directory(path) {
            return Err(MkdirError::AlreadyExists);
        }
        if !path
            .parent()
            .is_some_and(|parent| self.has_directory(parent))
        {
            return Err(MkdirError::NotFound);
        }
        self.directories.insert(path.to_owned());
        Ok(())
    }

    fn close(&mut self, handle: FsHandle) -> Result<(), CloseError> {
        self.open_files
            .remove(&handle)
            .map(|_| ())
            .ok_or(CloseError::NotOpen)
    }

    fn read(
        &mut self,
        handle: FsHandle,
        buf: &mut [u8],
        offset: usize,
    ) -> Result<usize, ReadError> {
        let path = self.open_files.get(&handle).ok_or(FsError::InvalidHandle)?;

        // file can't be deleted while it's open, so if we have a handle, it must exist in `self.files`
        let file = self.files.get(path).unwrap();

        let guard = file.read();
        let data = guard.as_slice();
        let file_len = data.len();
        if offset >= file_len {
            return Ok(0);
        }

        let bytes_to_read = file_len - offset;
        let bytes_to_copy = buf.len().min(bytes_to_read);
        buf[..bytes_to_copy].copy_from_slice(&data[offset..offset + bytes_to_copy]);
        Ok(bytes_to_copy)
    }

    fn write(&mut self, handle: FsHandle, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        let path = self.open_files.get(&handle).ok_or(FsError::InvalidHandle)?;

        // file can't be deleted while it's open, so if we have a handle, it must exist in `self.files`
        let file = self.files.get(path).unwrap();

        let mut guard = file.write();
        let file_len = guard.len();
        let need_max_len = offset + buf.len();
        if need_max_len > file_len {
            guard.resize(need_max_len, 0);
        }

        guard[offset..offset + buf.len()].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn truncate(&mut self, handle: FsHandle, len_bytes: u64) -> Result<(), TruncateError> {
        let path = self.open_files.get(&handle).ok_or(FsError::InvalidHandle)?;
        let len_bytes = usize::try_from(len_bytes).map_err(|_| TruncateError::TruncateFailed)?;
        self.files[path].write().resize(len_bytes, 0);
        Ok(())
    }

    fn read_dir(&mut self, handle: FsHandle) -> Result<Vec<String>, ReadDirError> {
        let dir: &AbsolutePath = self
            .open_files
            .get(&handle)
            .ok_or(FsError::InvalidHandle)?
            .as_ref();
        if self.files.contains_key(dir) {
            return Err(ReadDirError::NotDirectory);
        }
        let children: BTreeSet<&str> = self
            .files
            .keys()
            .chain(&self.directories)
            .filter_map(|path| {
                successors(Some(path.as_ref()), |p| p.parent()).find(|p| p.parent() == Some(dir))
            })
            .filter_map(|child| child.file_name())
            .collect();
        Ok([".", ".."]
            .into_iter()
            .chain(children)
            .map(ToOwned::to_owned)
            .collect())
    }

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError> {
        let path = self.open_files.get(&handle).ok_or(FsError::InvalidHandle)?;
        stat.size = self.files[path].read().len();
        Ok(())
    }

    fn fsync(&mut self, _handle: FsHandle) -> Result<(), FsyncError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::fs::{FileSystem, FsHandle};
    use crate::path::{AbsoluteOwnedPath, AbsolutePath};
    use crate::testing::TestFs;
    use crate::{CloseError, CreateMode, FsError, ReadDirError, TruncateError};

    #[test]
    fn test_open_close() {
        let mut fs = TestFs::default();
        fs.files.insert(
            AbsoluteOwnedPath::try_from("/foo").unwrap(),
            Default::default(),
        );

        assert!(
            fs.open(AbsolutePath::try_new("/bar").unwrap(), CreateMode::Never)
                .is_err()
        );
        let handle = fs
            .open(AbsolutePath::try_new("/foo").unwrap(), CreateMode::Never)
            .unwrap();

        assert!(fs.close(handle).is_ok());
        assert_eq!(Err(CloseError::NotOpen), fs.close(handle));
    }

    #[test]
    fn truncate_on_closed_handle_is_invalid_handle() {
        let mut fs = TestFs::default();
        fs.insert_file(AbsolutePath::try_new("/foo").unwrap(), b"ab".to_vec());
        let handle = fs
            .open(AbsolutePath::try_new("/foo").unwrap(), CreateMode::Never)
            .unwrap();
        fs.close(handle).unwrap();

        assert_eq!(
            Err(TruncateError::FsError(FsError::InvalidHandle)),
            fs.truncate(handle, 0)
        );
        assert_eq!(
            Err(TruncateError::FsError(FsError::InvalidHandle)),
            fs.truncate(FsHandle::from(u64::MAX), 0)
        );
    }

    #[test]
    fn read_dir_lists_only_direct_children() {
        let mut fs = TestFs::default();
        fs.insert_file(AbsolutePath::try_new("/d/a").unwrap(), Vec::new());
        fs.insert_file(AbsolutePath::try_new("/d/sub/nested").unwrap(), Vec::new());
        let dir = fs
            .open(AbsolutePath::try_new("/d").unwrap(), CreateMode::Never)
            .unwrap();
        let file = fs
            .open(AbsolutePath::try_new("/d/a").unwrap(), CreateMode::Never)
            .unwrap();

        assert_eq!(
            Ok(vec![
                ".".to_owned(),
                "..".to_owned(),
                "a".to_owned(),
                "sub".to_owned()
            ]),
            fs.read_dir(dir)
        );
        assert_eq!(Err(ReadDirError::NotDirectory), fs.read_dir(file));
    }
}
