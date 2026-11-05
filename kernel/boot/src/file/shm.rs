use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use core::alloc::Layout;
use core::ptr::copy_nonoverlapping;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::{ProcessId, S_IFREG};
use kernel_memapi::{Guarded, Location, MemoryApi};
use kernel_vfs::fs::{FileSystem, FsHandle};
use kernel_vfs::path::{AbsolutePath, ROOT};
use kernel_vfs::{
    CloseError, CreateMode, FsError, FsyncError, MappedMemory, MkdirError, MmapError, MmapRegion,
    OpenError, ReadError, Stat, StatError, TruncateError, UnlinkError, WriteError,
};
use spin::Mutex;
use x86_64::structures::paging::{PageSize, Size4KiB};

use crate::U64Ext;
use crate::mcore::mtask::process::Process;
use crate::mem::memapi::{FrameContiguity, HigherHalfAllocation, HigherHalfMemoryApi, Writable};

/// An object exists until a process unlinks its name and the last handle on
/// it closes. Each mapping holds a share of the frames, so the frames stay
/// valid until the last mapping drops.
///
/// Only the creating process opens or unlinks an object by name.
#[derive(Default)]
pub struct ShmFs {
    objects: BTreeMap<String, ShmObject>,
    handles: BTreeMap<FsHandle, Arc<Mutex<ShmState>>>,
}

struct ShmObject {
    owner: ProcessId,
    state: Arc<Mutex<ShmState>>,
}

#[derive(Default)]
struct ShmState {
    size: usize,
    /// The bytes are in the kernel half. The kernel copies them directly,
    /// and every mapping holds a clone of this `Arc`, so the bytes stay
    /// valid for as long as any mapping exists.
    ///
    /// TODO(tpdenk): allocate the frames in several ranges once a large
    /// object fails to allocate.
    backing: Option<Arc<HigherHalfAllocation<Writable>>>,
}

impl ShmState {
    fn bytes(&self) -> &[u8] {
        self.backing
            .as_ref()
            .map_or(&[], |backing| &(**backing).as_ref()[..self.size])
    }

    fn read(&self, buf: &mut [u8], offset: usize) -> usize {
        let bytes = self.bytes();
        if offset >= bytes.len() {
            return 0;
        }
        let len = buf.len().min(bytes.len() - offset);
        buf[..len].copy_from_slice(&bytes[offset..offset + len]);
        len
    }

    fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        let backing = self
            .backing
            .as_ref()
            .filter(|_| offset < self.size)
            .ok_or(WriteError::NoSpace)?;
        let len = buf.len().min(self.size - offset);
        // SAFETY: `offset + len <= size <= backing.len()`. The state lock
        // serializes kernel writers. User mappings alias the bytes by design,
        // so no `&mut [u8]` can exist and the copy goes through the raw pointer.
        unsafe { copy_nonoverlapping(buf.as_ptr(), backing.ptr().as_ptr().add(offset), len) };
        Ok(len)
    }

    fn resize(&mut self, len: usize) -> Result<(), TruncateError> {
        // Every mapping holds one clone, so the frames cannot change while
        // a mapping points at them.
        if self
            .backing
            .as_ref()
            .is_some_and(|b| Arc::strong_count(b) > 1)
        {
            return Err(TruncateError::Busy);
        }
        if len == self.size {
            return Ok(());
        }
        let backing = if len == 0 {
            None
        } else {
            let layout = Layout::from_size_align(len, Size4KiB::SIZE.into_usize())
                .map_err(|_| TruncateError::TruncateFailed)?;
            let mut backing = HigherHalfMemoryApi
                .allocate(
                    Location::Anywhere,
                    layout,
                    FrameContiguity::Contiguous,
                    Guarded::No,
                )
                .ok_or(TruncateError::TruncateFailed)?;
            let kept = self.size.min(len);
            let bytes = backing.as_mut();
            bytes[..kept].copy_from_slice(&self.bytes()[..kept]);
            bytes[kept..].fill(0);
            Some(Arc::new(backing))
        };
        self.backing = backing;
        self.size = len;
        Ok(())
    }

    fn map(&self) -> Result<MmapRegion, MmapError> {
        let backing = self.backing.as_ref().ok_or(MmapError::NotSupported)?;
        Ok(backing.clone())
    }
}

fn object_name(path: &AbsolutePath) -> Option<&str> {
    (path.parent() == Some(ROOT))
        .then(|| path.file_name())
        .flatten()
}

impl ShmFs {
    fn open_handle(&self, handle: FsHandle) -> Result<&Mutex<ShmState>, FsError> {
        self.handles
            .get(&handle)
            .map(Arc::as_ref)
            .ok_or(FsError::InvalidHandle)
    }
}

impl FileSystem for ShmFs {
    fn open(&mut self, path: &AbsolutePath, create: CreateMode) -> Result<FsHandle, OpenError> {
        static FS_COUNTER: AtomicU64 = AtomicU64::new(0);

        let name = object_name(path).ok_or(OpenError::NotFound)?;
        let caller = Process::current().pid();
        let object = match (self.objects.get(name), create) {
            (Some(object), _) if object.owner != caller => {
                return Err(OpenError::PermissionDenied);
            }
            (Some(_), CreateMode::Exclusive) => return Err(OpenError::AlreadyExists),
            (Some(object), CreateMode::Never | CreateMode::IfMissing) => object.state.clone(),
            (None, CreateMode::Never) => return Err(OpenError::NotFound),
            (None, CreateMode::IfMissing | CreateMode::Exclusive) => {
                let state = Arc::<Mutex<ShmState>>::default();
                self.objects.insert(
                    name.into(),
                    ShmObject {
                        owner: caller,
                        state: state.clone(),
                    },
                );
                state
            }
        };
        let handle = FsHandle::from(FS_COUNTER.fetch_add(1, Relaxed));
        self.handles.insert(handle, object);
        Ok(handle)
    }

    fn unlink(&mut self, path: &AbsolutePath) -> Result<(), UnlinkError> {
        let name = object_name(path).ok_or(UnlinkError::NotFound)?;
        let object = self.objects.get(name).ok_or(UnlinkError::NotFound)?;
        if object.owner != Process::current().pid() {
            return Err(UnlinkError::PermissionDenied);
        }
        self.objects.remove(name);
        Ok(())
    }

    fn create_directory(&mut self, _path: &AbsolutePath) -> Result<(), MkdirError> {
        Err(MkdirError::NotSupported)
    }

    fn close(&mut self, handle: FsHandle) -> Result<(), CloseError> {
        self.handles
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
        Ok(self.open_handle(handle)?.lock().read(buf, offset))
    }

    fn write(&mut self, handle: FsHandle, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        self.open_handle(handle)?.lock().write(buf, offset)
    }

    fn truncate(&mut self, handle: FsHandle, len_bytes: u64) -> Result<(), TruncateError> {
        let len = usize::try_from(len_bytes).map_err(|_| TruncateError::TruncateFailed)?;
        self.open_handle(handle)?.lock().resize(len)
    }

    fn stat(&mut self, handle: FsHandle, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = self.open_handle(handle)?.lock().size;
        stat.mode = S_IFREG | 0o600;
        Ok(())
    }

    fn mmap(&mut self, handle: FsHandle) -> Result<MmapRegion, MmapError> {
        self.open_handle(handle)?.lock().map()
    }

    fn fsync(&mut self, _handle: FsHandle) -> Result<(), FsyncError> {
        Ok(())
    }
}
