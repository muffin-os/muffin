use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::{
    EACCES, EAGAIN, EBADF, EBUSY, EEXIST, EFBIG, EINTR, EINVAL, EIO, EISDIR, ENAMETOOLONG, ENODEV,
    ENOENT, ENOMEM, ENOSPC, ENOSYS, ENOTDIR, ENOTTY, EPERM, EPIPE, ESPIPE, Errno, FdNum,
    IoctlRequest, ProtFlags, S_IFSOCK, SigInfo, SigInfoField, Signal, Stat,
};
use kernel_syscall::access::{
    AccessMode, AllocationStrategy, CreateMappingError, CwdAccess, FileAccess, Location,
    MemoryRegion as _, SignalAccess,
};
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{
    CreateMode, FdLookup, FsError, FsyncError, IoctlError, MmapError, MmapRegion, OpenError,
    ReadDirError, ReadError, Stat as VfsStat, TruncateError, UnlinkError, WaitSource, WriteError,
};
use kernel_virtual_memory::Segment;
use spin::rwlock::RwLock;
use x86_64::VirtAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PageTableFlags, PhysFrame, Size4KiB};

use crate::file::{OpenFile, OpenFileDescription, vfs};
use crate::mcore::mtask::process::fd::{FileDescriptor, FileDescriptorFlags};
use crate::mcore::mtask::process::mem::{
    FileBackedMemoryRegion, MemoryRegion, PrivateMemoryRegion, SetProtectionError,
    SharedMemoryRegion, SplitError,
};
use crate::mcore::mtask::process::{ParkOutcome, Process};
use crate::mcore::mtask::task::Task;
use crate::mcore::mtask::wait::WaitQueue;
use crate::mem::address_space::AddressSpace;
use crate::mem::virt::VirtualMemoryAllocator;
use crate::{U64Ext, UsizeExt};

mod poll;
mod signal;
mod socket;
mod thread;
mod wait;

pub struct KernelAccess {
    process: Arc<Process>,
}

impl KernelAccess {
    #[expect(
        clippy::new_without_default,
        reason = "the value depends on the current task, so a default value misleads"
    )]
    pub fn new() -> Self {
        KernelAccess {
            process: Process::current().clone(),
        }
    }

    /// Installs `ofd` on the lowest free descriptor.
    fn install(&self, ofd: Arc<OpenFileDescription>) -> FdNum {
        let num = {
            let mut table = self.process.file_descriptors().write();
            let num = table
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
            table.insert(num, fd);
            self.process.advance_fd_generation(&mut table);
            num
        };
        self.process.fd_changes().wake_all();
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
        let task = Task::current();
        while matches!(result, Err(EAGAIN)) {
            let outcome = self.process.park_current_task(None, &[queue], || {
                result = op();
                !matches!(result, Err(EAGAIN)) || task.has_interrupting_signal()
            });
            if matches!(outcome, ParkOutcome::Interrupted) || matches!(result, Err(EAGAIN)) {
                return Err(EINTR);
            }
        }
        result
    }

    fn broken_pipe(&self) -> Errno {
        // POSIX: a write to a socket whose peer is gone raises SIGPIPE and
        // fails with EPIPE.
        SignalAccess::deliver(
            self,
            self.process.pid(),
            SigInfo::new(Signal::Pipe, SigInfoField::None),
        );
        EPIPE
    }
}

/// Advances the offset after a completed read and maps its error to an errno.
fn finish_read(
    ofd: &OpenFile,
    offset: u64,
    result: Result<usize, ReadError>,
) -> Result<usize, Errno> {
    let read = result.map_err(|e| match e {
        ReadError::WouldBlock => EAGAIN,
        ReadError::NotReadable => EISDIR,
        ReadError::BufferTooSmall => EINVAL,
        ReadError::FsError(_) | ReadError::ReadFailed => EIO,
    })?;
    // The load and the store are separate. A fork shares the description
    // between processes, so a concurrent read can lose an offset update.
    ofd.position().store(offset + read.into_u64(), Relaxed);
    Ok(read)
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
            OpenError::PermissionDenied => EACCES,
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
            UnlinkError::PermissionDenied => EACCES,
        })
    }

    fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, Errno> {
        loop {
            let fds = self.process.file_descriptors();
            let guard = fds.read();

            let desc = guard.get(&fd).ok_or(EBADF)?;
            let description = desc.file_description().clone();
            let generation = self.process.fd_generation();
            let ofd = match description.as_ref() {
                OpenFileDescription::File(ofd) => ofd,
                OpenFileDescription::Socket(socket) => {
                    let socket = socket.clone();
                    // The wait must not hold the descriptor table lock.
                    drop(guard);
                    return self.block_on(socket.waiters(), || socket.try_read(buf));
                }
            };
            if !ofd.access().can_read() {
                return Err(EBADF);
            }
            let offset = ofd.position().load(Relaxed);
            let result = ofd.read(&mut *buf, offset.into_usize());
            // The wait must not hold the descriptor table lock.
            drop(guard);
            if !matches!(result, Err(ReadError::WouldBlock)) {
                return finish_read(ofd, offset, result);
            }

            let device = description.wait_source().map_err(|_| EIO)?;
            let fd_changes: &dyn WaitSource = self.process.fd_changes();
            let both;
            let sources: &[&dyn WaitSource] = match device.as_deref() {
                Some(device) => {
                    both = [device, fd_changes];
                    &both
                }
                None => core::slice::from_ref(&fd_changes),
            };
            let task = Task::current();
            let mut is_replaced = false;
            let mut attempt = None;
            let outcome = self.process.park_current_task(None, sources, || {
                let guard = fds.read();
                if self.process.fd_generation() != generation {
                    drop(guard);
                    is_replaced = true;
                    return true;
                }
                let offset = ofd.position().load(Relaxed);
                let result = ofd.read(&mut *buf, offset.into_usize());
                drop(guard);
                let is_blocked = matches!(result, Err(ReadError::WouldBlock));
                attempt = Some((offset, result));
                !is_blocked || task.has_interrupting_signal()
            });
            // A completed read consumes data, so it wins over an interruption.
            if let Some((offset, result)) = attempt
                && !matches!(result, Err(ReadError::WouldBlock))
            {
                return finish_read(ofd, offset, result);
            }
            if matches!(outcome, ParkOutcome::Interrupted) || !is_replaced {
                return Err(EINTR);
            }
        }
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
                Err(EPIPE) => return Err(self.broken_pipe()),
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }

    fn close(&self, fd: Self::Fd) -> Result<(), Errno> {
        let removed = {
            let mut table = self.process.file_descriptors().write();
            let removed = table.remove(&fd).ok_or(EBADF)?;
            self.process.advance_fd_generation(&mut table);
            removed
        };
        self.process.fd_changes().wake_all();
        // Its destructor can notify socket peers, so it runs without the table lock.
        drop(removed);
        Ok(())
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
        // Release the fd table lock before the call, so that the lookup can take it again.
        let file = {
            let fds = self.process.file_descriptors();
            let guard = fds.read();
            guard.get(&fd).ok_or(EBADF)?.file_description().clone()
        };
        file.file()
            .ok_or(ENOTTY)?
            .ioctl(request, arg, self)
            .map_err(|e| match e {
                IoctlError::NotSupported => ENOTTY,
                IoctlError::InvalidArgument | IoctlError::FsError(_) => EINVAL,
                IoctlError::Busy => EBUSY,
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
                    VirtAddr::new(addr.addr().into_u64()),
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
        let memory = self.mmap_region(fd)?;

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
        let range = user_range(addr, len).ok_or(EINVAL)?;
        let _exclusive = self.process.exclusive_user_memory();
        let regions = self.process.memory_regions();
        // The drop of the last `Arc` frees the frames of a private region.
        // For a shared region, the drop releases the file's mapping.
        for region in regions.carve(&range).map_err(split_errno)? {
            regions.remove_region(self.process.address_space(), &region);
        }
        Ok(())
    }

    fn protect(
        &self,
        addr: kernel_syscall::UserspacePtr<u8>,
        len: usize,
        prot: ProtFlags,
    ) -> Result<(), Errno> {
        let range = user_range(addr, len).ok_or(ENOMEM)?;
        let _exclusive = self.process.exclusive_user_memory();
        let regions = self.process.memory_regions();
        if !regions.covers(&range) {
            return Err(ENOMEM);
        }
        let flags = page_flags(prot);
        for region in regions.carve(&range).map_err(split_errno)? {
            region
                .set_protection(self.process.address_space(), flags)
                .map_err(|e| match e {
                    SetProtectionError::UnsupportedRegion => EACCES,
                    SetProtectionError::RemapFailed => EINVAL,
                })?;
        }
        Ok(())
    }
}

/// This function returns the addresses `[addr, addr + len)` of a user range.
/// It returns `None` when the range leaves the lower half.
fn user_range(addr: kernel_syscall::UserspacePtr<u8>, len: usize) -> Option<Range<u64>> {
    addr.validate_range(len).ok()?;
    let start = addr.addr().into_u64();
    Some(start..start + len.into_u64())
}

fn split_errno(error: SplitError) -> Errno {
    match error {
        SplitError::EagerBacking | SplitError::Shared | SplitError::InUse => EINVAL,
    }
}

/// This function grants user access only for a `prot` that allows some
/// access. A `PROT_NONE` page then faults on each user access.
fn page_flags(prot: ProtFlags) -> PageTableFlags {
    PageTableFlags::PRESENT
        | if prot.intersects(ProtFlags::READ | ProtFlags::WRITE | ProtFlags::EXEC) {
            PageTableFlags::USER_ACCESSIBLE
        } else {
            PageTableFlags::empty()
        }
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

impl FdLookup for KernelAccess {
    fn mmap_region(&self, fd: FdNum) -> Result<MmapRegion, Errno> {
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
            })
    }
}
