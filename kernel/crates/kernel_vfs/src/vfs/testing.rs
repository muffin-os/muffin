use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

use spin::RwLock;

use crate::fs::{FileSystem, FsHandle};
use crate::path::{AbsoluteOwnedPath, AbsolutePath};
use crate::{CloseError, FsError, OpenError, ReadError, Stat, StatError, WriteError};

#[derive(Default)]
pub struct TestFs {
    handle_counter: AtomicU64,
    files: BTreeMap<AbsoluteOwnedPath, RwLock<Vec<u8>>>,
    stats: BTreeMap<AbsoluteOwnedPath, Stat>,
    open_files: BTreeMap<FsHandle, AbsoluteOwnedPath>,
}

impl TestFs {
    pub fn insert_file(&mut self, path: impl AsRef<AbsolutePath>, data: Vec<u8>, stat: Stat) {
        let path = path.as_ref().to_owned();
        self.files.insert(path.clone(), RwLock::new(data));
        self.stats.insert(path, stat);
    }
}

impl FileSystem for TestFs {
    fn open(&mut self, path: &AbsolutePath) -> Result<FsHandle, OpenError> {
        let owned = path.to_owned();
        if self.files.contains_key(&owned) {
            let handle = FsHandle::from(self.handle_counter.fetch_add(1, Relaxed));
            self.open_files.insert(handle, owned.clone());
            Ok(handle)
        } else {
            Err(OpenError::NotFound)
        }
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

    fn stat(&mut self, _handle: FsHandle, _stat: &mut Stat) -> Result<(), StatError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use crate::CloseError;
    use crate::fs::FileSystem;
    use crate::path::{AbsoluteOwnedPath, AbsolutePath};
    use crate::testing::TestFs;

    #[test]
    fn test_open_close() {
        let mut fs = TestFs::default();
        fs.files.insert(
            AbsoluteOwnedPath::try_from("/foo").unwrap(),
            Default::default(),
        );

        assert!(fs.open(AbsolutePath::try_new("/bar").unwrap()).is_err());
        let handle = fs.open(AbsolutePath::try_new("/foo").unwrap()).unwrap();

        assert!(fs.close(handle).is_ok());
        assert_eq!(Err(CloseError::NotOpen), fs.close(handle));
    }
}
