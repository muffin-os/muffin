use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::{
    EACCES, EAGAIN, EBADF, EBUSY, EEXIST, EFBIG, EINTR, EINVAL, EIO, EISDIR, ENAMETOOLONG, ENODEV,
    ENOENT, ENOMEM, ENOSPC, ENOSYS, ENOTDIR, ENOTTY, EPERM, EPIPE, ESPIPE, Errno, IoctlRequest,
    ProtFlags, S_IFSOCK, SigInfo, SigInfoField, Signal, Stat,
};
use kernel_syscall::access::{
    AccessMode, AllocationStrategy, CreateMappingError, CwdAccess, FileAccess, Location,
    MemoryRegion as _, SignalAccess,
};
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{
    CreateMode, FsError, FsyncError, IoctlError, MmapError, OpenError, ReadDirError, ReadError,
    Stat as VfsStat, TruncateError, UnlinkError, WriteError,
};
use kernel_virtual_memory::Segment;
use spin::rwlock::RwLock;
use x86_64::VirtAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PageTableFlags, PhysFrame, Size4KiB};

use crate::file::{OpenFile, OpenFileDescription, vfs};
use crate::mcore::mtask::process::fd::{FdNum, FileDescriptor, FileDescriptorFlags};
use crate::mcore::mtask::process::mem::{
    FileBackedMemoryRegion, MemoryRegion, PrivateMemoryRegion, SharedMemoryRegion,
};
use crate::mcore::mtask::process::{ParkOutcome, Process};
use crate::mcore::mtask::wait::WaitQueue;
use crate::mem::address_space::AddressSpace;
use crate::mem::virt::VirtualMemoryAllocator;
use crate::{U64Ext, UsizeExt};

mod signal;
mod socket;
mod wait;

pub struct KernelAccess {
    process: Arc<Process>,
}

impl KernelAccess {
    pub fn new() -> Self {
        KernelAccess {
            process: Process::current().clone(),
        }
    }

    /// Installs `ofd` on the lowest free descriptor.
    fn install(&self, ofd: Arc<OpenFileDescription>) -> FdNum {
        let num = self
            .process
            .file_descriptors()
            .read()
            .keys()
            .fold(0, |acc, &fd| {
                if acc == Into::<i32>::into(fd) {
                    acc + 1
                } else {
                    acc
                }
            })
            .into();
        let fd = FileDescriptor::new(num, FileDescriptorFlags::empty(), ofd);

        self.process.file_descriptors().write().insert(num, fd);

        num
    }

    /// Retries `op` while it would block. Between tries, the task parks on `queue`.
    /// Returns `EINTR` when a signal or an exec reap ends the wait first.
    fn block_on<T>(
        &self,
        queue: &WaitQueue,
        mut op: impl FnMut() -> Result<T, Errno>,
    ) -> Result<T, Errno> {
        let mut result = op();
        while matches!(result, Err(EAGAIN)) {
            let outcome = self.process.park_current_task(None, Some(queue), || {
                result = op();
                !matches!(result, Err(EAGAIN))
                    || self.process.signals_read().has_interrupting_deliverable()
            });
            if matches!(outcome, ParkOutcome::Interrupted) || matches!(result, Err(EAGAIN)) {
                return Err(EINTR);
            }
        }
        result
    }
}

impl CwdAccess for KernelAccess {
    fn current_working_directory(&self) -> &RwLock<kernel_vfs::path::AbsoluteOwnedPath> {
        self.process.current_working_directory()
    }
}

pub struct FileInfo {
    node: VfsNode,
}

impl kernel_syscall::access::FileInfo for FileInfo {}

impl FileAccess for KernelAccess {
    type FileInfo = FileInfo;
    type Fd = FdNum;

    fn file_info(&self, path: &AbsolutePath, create: CreateMode) -> Result<Self::FileInfo, Errno> {
        let node = vfs().read().open(path, create).map_err(|e| match e {
            OpenError::NotFound => ENOENT,
            OpenError::OpenFailed => EIO,
            OpenError::AlreadyExists => EEXIST,
            OpenError::NoSpace => ENOSPC,
            OpenError::CreateUnsupported => EACCES,
            OpenError::NameTooLong => ENAMETOOLONG,
        })?;
        Ok(FileInfo { node })
    }

    fn open(&self, info: &Self::FileInfo, access: AccessMode, append: bool) -> Self::Fd {
        let ofd = OpenFile::new(info.node.clone(), access, append);
        self.install(Arc::new(OpenFileDescription::File(ofd)))
    }

    fn unlink(&self, path: &AbsolutePath) -> Result<(), Errno> {
        vfs().read().unlink(path).map_err(|e| match e {
            UnlinkError::NotFound => ENOENT,
            UnlinkError::NotSupported => EPERM,
            UnlinkError::FsError(_) => EIO,
        })
    }

    fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let socket = match desc.file_description().as_ref() {
            OpenFileDescription::File(ofd) => {
                if !ofd.access().can_read() {
                    return Err(EBADF);
                }
                let offset = ofd.position().load(Relaxed);
                let read = ofd.read(buf, offset.into_usize()).map_err(|e| match e {
                    ReadError::NotReadable => EISDIR,
                    ReadError::FsError(_) | ReadError::ReadFailed => EIO,
                })?;
                // The load and the store are separate. A fork shares the description
                // between processes, so a concurrent read can lose an offset update.
                ofd.position().store(offset + read.into_u64(), Relaxed);
                return Ok(read);
            }
            OpenFileDescription::Socket(socket) => socket.clone(),
        };
        // The wait must not hold the descriptor table lock.
        drop(guard);
        self.block_on(socket.waiters(), || socket.try_read(buf))
    }

    fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let socket = match desc.file_description().as_ref() {
            OpenFileDescription::File(ofd) => {
                if !ofd.access().can_write() {
                    return Err(EBADF);
                }
                let offset = if ofd.is_append() {
                    let mut stat = VfsStat::default();
                    ofd.stat(&mut stat).map_err(|_| EIO)?;
                    stat.size.into_u64()
                } else {
                    ofd.position().load(Relaxed)
                };
                let written = ofd.write(buf, offset.into_usize()).map_err(|e| match e {
                    WriteError::NotWritable => EISDIR,
                    WriteError::NoSpace => ENOSPC,
                    WriteError::FileTooLarge => EFBIG,
                    WriteError::FsError(_) | WriteError::WriteFailed => EIO,
                })?;
                ofd.position().store(offset + written.into_u64(), Relaxed);
                return Ok(written);
            }
            OpenFileDescription::Socket(socket) => socket.clone(),
        };
        // The wait must not hold the descriptor table lock.
        drop(guard);
        let mut done = 0;
        while done < buf.len() {
            match self.block_on(socket.waiters(), || socket.try_write(&buf[done..])) {
                Ok(n) => done += n,
                // POSIX reports a partial count when some bytes reached the peer.
                Err(_) if done > 0 => break,
                Err(EPIPE) => {
                    SignalAccess::deliver(
                        self,
                        self.process.pid(),
                        SigInfo {
                            signo: Signal::Pipe,
                            code: 0,
                            errno: 0,
                            info: SigInfoField::None,
                        },
                    );
                    return Err(EPIPE);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }

    fn close(&self, fd: Self::Fd) -> Result<(), Errno> {
        self.process
            .file_descriptors()
            .write()
            .remove(&fd)
            .map(|_| ())
            .ok_or(EBADF)
    }

    fn truncate(&self, fd: Self::Fd, len_bytes: u64) -> Result<(), Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let file = desc.file_description().file().ok_or(EINVAL)?;
        if !file.access().can_write() {
            return Err(EINVAL);
        }
        file.truncate(len_bytes).map_err(|e| match e {
            TruncateError::NotSupported => EINVAL,
            TruncateError::Busy => EBUSY,
            TruncateError::FsError(FsError::InvalidHandle) => EBADF,
            TruncateError::FsError(FsError::FileSystemNotOpen) | TruncateError::TruncateFailed => {
                EIO
            }
        })
    }

    fn read_dir(&self, fd: Self::Fd) -> Result<Vec<String>, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let file = desc.file_description().file().ok_or(ENOTDIR)?;
        file.read_dir().map_err(|e| match e {
            ReadDirError::NotDirectory => ENOTDIR,
            ReadDirError::NotSupported => ENOSYS,
            ReadDirError::ReadFailed => EIO,
            ReadDirError::FsError(_) => EBADF,
        })
    }

    fn ioctl(&self, fd: Self::Fd, request: IoctlRequest, arg: &mut [u8]) -> Result<usize, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        desc.file_description()
            .file()
            .ok_or(ENOTTY)?
            .ioctl(request, arg)
            .map_err(|e| match e {
                IoctlError::NotSupported => ENOTTY,
                IoctlError::InvalidArgument | IoctlError::FsError(_) => EINVAL,
            })
    }

    fn fsync(&self, fd: Self::Fd) -> Result<(), Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let file = desc.file_description().file().ok_or(EINVAL)?;
        file.fsync().map_err(|e| match e {
            FsyncError::FsError(_) | FsyncError::Failed => EIO,
        })
    }

    fn fstat(&self, fd: Self::Fd) -> Result<Stat, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let Some(file) = desc.file_description().file() else {
            return Ok(Stat {
                size: 0,
                mode: S_IFSOCK | 0o777,
            });
        };
        let mut stat = VfsStat::default();
        file.stat(&mut stat).map_err(|_| EIO)?;
        Ok(Stat {
            size: stat.size.into_u64(),
            mode: stat.mode,
        })
    }

    fn position(&self, fd: Self::Fd) -> Result<u64, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let file = desc.file_description().file().ok_or(ESPIPE)?;
        Ok(file.position().load(Relaxed))
    }

    fn set_position(&self, fd: Self::Fd, position: u64) -> Result<(), Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let file = desc.file_description().file().ok_or(ESPIPE)?;
        file.position().store(position, Relaxed);
        Ok(())
    }
}

impl kernel_syscall::access::MemoryRegionAccess for KernelAccess {
    type Region = KernelMemoryRegionHandle;
    type Fd = FdNum;

    fn create_and_track_mapping(
        &self,
        location: Location,
        size: usize,
        _allocation_strategy: AllocationStrategy,
        prot: ProtFlags,
    ) -> Result<kernel_syscall::UserspacePtr<u8>, CreateMappingError> {
        let page_aligned_size = size.next_multiple_of(Size4KiB::SIZE as usize);
        let page_count = page_aligned_size / Size4KiB::SIZE as usize;

        let segment = if let Location::Fixed(addr) = location {
            self.process
                .vmm()
                .mark_as_reserved(Segment::new(
                    VirtAddr::from_ptr(addr.as_ptr()),
                    page_aligned_size.into_u64(),
                ))
                .map_err(|_| CreateMappingError::LocationAlreadyMapped)?
        } else {
            self.process
                .vmm()
                .reserve(page_count)
                .ok_or(CreateMappingError::OutOfMemory)?
        };

        // The region spans the page-aligned size so the fault handler can
        // serve the last partial page.
        let region = KernelMemoryRegionHandle {
            inner: MemoryRegion::Private(PrivateMemoryRegion::new(
                segment,
                page_aligned_size,
                page_flags(prot),
            )),
        };
        let addr = region.addr();
        self.add_memory_region(region);

        Ok(addr)
    }

    fn add_memory_region(&self, region: Self::Region) {
        self.process.memory_regions().add_region(region.inner);
    }

    fn map_shared_file(
        &self,
        fd: FdNum,
        len: usize,
        prot: ProtFlags,
    ) -> Result<kernel_syscall::UserspacePtr<u8>, Errno> {
        let memory = {
            let fds = self.process.file_descriptors();
            let guard = fds.read();
            let desc = guard.get(&fd).ok_or(EBADF)?;
            desc.file_description()
                .file()
                .ok_or(ENODEV)?
                .mmap()
                .map_err(|e| match e {
                    MmapError::NotSupported => ENODEV,
                    MmapError::FsError(_) => EINVAL,
                })?
        };

        let page_size = Size4KiB::SIZE as usize;
        let page_aligned = len.next_multiple_of(page_size);
        if page_aligned > memory.len().next_multiple_of(page_size) {
            return Err(EINVAL);
        }

        // The memory pointer is a kernel virtual address. Translate it to the
        // physical base of the contiguous frames.
        let phys = AddressSpace::kernel()
            .translate(VirtAddr::from_ptr(memory.ptr().as_ptr()))
            .ok_or(EINVAL)?;
        if !phys.is_aligned(Size4KiB::SIZE) {
            return Err(EINVAL);
        }

        let page_count = page_aligned / page_size;
        let start = PhysFrame::<Size4KiB>::containing_address(phys);
        let frames = PhysFrameRangeInclusive {
            start,
            end: start + (page_count as u64 - 1),
        };

        let segment = self.process.vmm().reserve(page_count).ok_or(ENOMEM)?;
        let addr = segment.start;

        self.process
            .address_space()
            .map_range::<Size4KiB>(&*segment, frames, page_flags(prot))
            .map_err(|_| EINVAL)?;

        let user_ptr = addr.as_ptr::<u8>().try_into().map_err(|_| EINVAL)?;

        // The region owns the virtual reservation and a share of the memory,
        // but not the physical frames, so process exit never frees them.
        let inner = MemoryRegion::Shared(SharedMemoryRegion::new(
            segment,
            page_aligned,
            frames,
            memory,
        ));
        self.add_memory_region(KernelMemoryRegionHandle { inner });

        Ok(user_ptr)
    }

    fn map_private_file(
        &self,
        fd: FdNum,
        offset: usize,
        len: usize,
        prot: ProtFlags,
    ) -> Result<kernel_syscall::UserspacePtr<u8>, Errno> {
        let page_size = Size4KiB::SIZE.into_usize();
        // The mapping starts on a page boundary, so an unaligned file offset has
        // no address to land on.
        if !offset.is_multiple_of(page_size) {
            return Err(EINVAL);
        }

        let node = {
            let fds = self.process.file_descriptors();
            let guard = fds.read();
            let desc = guard.get(&fd).ok_or(EBADF)?;
            VfsNode::clone(desc.file_description().file().ok_or(ENODEV)?)
        };

        let mut stat = VfsStat::default();
        node.stat(&mut stat).map_err(|_| EIO)?;
        // Bytes past the end of the file read as zero, so a mapping reaching
        // beyond the end is zero filled rather than rejected.
        let file_len = stat.size.saturating_sub(offset).min(len);

        let page_count = len.div_ceil(page_size);
        let size = page_count * page_size;
        let segment = self.process.vmm().reserve(page_count).ok_or(ENOMEM)?;
        let addr = segment.start;

        let flags = page_flags(prot);

        let user_ptr = addr.as_ptr::<u8>().try_into().map_err(|_| EINVAL)?;
        // The region must carry the page-rounded size. `MemoryRegion::contains`
        // bounds the fault handler, so a region sized to `len` leaves the last
        // partial page unservable and any access to it kills the process.
        let private = PrivateMemoryRegion::new(segment, size, flags);
        self.add_memory_region(KernelMemoryRegionHandle {
            inner: MemoryRegion::FileBacked(FileBackedMemoryRegion::new(
                private, node, offset, file_len,
            )),
        });

        Ok(user_ptr)
    }

    fn unmap(&self, addr: kernel_syscall::UserspacePtr<u8>, len: usize) -> Result<(), Errno> {
        let regions = self.process.memory_regions();
        let region = regions
            .region_for(VirtAddr::new(addr.addr().into_u64()))
            .ok_or(EINVAL)?;
        if region.addr().as_u64() != addr.addr().into_u64()
            || region.size() != len.next_multiple_of(Size4KiB::SIZE.into_usize())
        {
            return Err(EINVAL);
        }
        // The drop of the last `Arc` frees the frames of a private region.
        // For a shared region, the drop releases the file's mapping.
        regions.remove_region(self.process.address_space(), &region);
        Ok(())
    }
}

fn page_flags(prot: ProtFlags) -> PageTableFlags {
    PageTableFlags::PRESENT
        | PageTableFlags::USER_ACCESSIBLE
        | if prot.contains(ProtFlags::WRITE) {
            PageTableFlags::WRITABLE
        } else {
            PageTableFlags::empty()
        }
        | if prot.contains(ProtFlags::EXEC) {
            PageTableFlags::empty()
        } else {
            PageTableFlags::NO_EXECUTE
        }
}

pub struct KernelMemoryRegionHandle {
    inner: MemoryRegion,
}

impl kernel_syscall::access::MemoryRegion for KernelMemoryRegionHandle {
    fn addr(&self) -> kernel_syscall::UserspacePtr<u8> {
        self.inner
            .addr()
            .as_ptr::<u8>()
            .try_into()
            .expect("memory region should be located in user space")
    }

    fn size(&self) -> usize {
        self.inner.size()
    }
}
