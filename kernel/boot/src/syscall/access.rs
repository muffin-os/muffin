use alloc::sync::Arc;
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::{
    EACCES, EBADF, EEXIST, EFBIG, EINVAL, EIO, EISDIR, ENAMETOOLONG, ENODEV, ENOENT, ENOMEM,
    ENOSPC, ENOTTY, Errno, IoctlRequest, ProtFlags, Stat,
};
use kernel_syscall::access::{
    AccessMode, AllocationStrategy, CreateMappingError, CwdAccess, FileAccess, Location,
    MemoryRegion as _,
};
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{
    CreateMode, FsError, FsyncError, IoctlError, MmapError, OpenError, ReadError, Stat as VfsStat,
    TruncateError, WriteError,
};
use kernel_virtual_memory::Segment;
use spin::rwlock::RwLock;
use x86_64::VirtAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PageTableFlags, PhysFrame, Size4KiB};

use crate::file::{OpenFileDescription, vfs};
use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::process::fd::{FdNum, FileDescriptor, FileDescriptorFlags};
use crate::mcore::mtask::process::mem::{
    FileBackedMemoryRegion, MemoryRegion, PrivateMemoryRegion, SharedMemoryRegion,
};
use crate::mem::address_space::AddressSpace;
use crate::mem::virt::VirtualMemoryAllocator;
use crate::{U64Ext, UsizeExt};

mod signal;
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
        let ofd = OpenFileDescription::new(info.node.clone(), access, append);
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
        let fd = FileDescriptor::new(num, FileDescriptorFlags::empty(), ofd.into());

        self.process.file_descriptors().write().insert(num, fd);

        num
    }

    fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let ofd = desc.file_description();
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
        Ok(read)
    }

    fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let ofd = desc.file_description();
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
        Ok(written)
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
        desc.file_description()
            .truncate(len_bytes)
            .map_err(|e| match e {
                TruncateError::NotSupported => EINVAL,
                TruncateError::FsError(FsError::InvalidHandle) => EBADF,
                TruncateError::FsError(FsError::FileSystemNotOpen)
                | TruncateError::TruncateFailed => EIO,
            })
    }

    fn ioctl(&self, fd: Self::Fd, request: IoctlRequest, arg: &mut [u8]) -> Result<usize, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        desc.file_description()
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
        desc.file_description().fsync().map_err(|e| match e {
            FsyncError::FsError(_) | FsyncError::Failed => EIO,
        })
    }

    fn fstat(&self, fd: Self::Fd) -> Result<Stat, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        let mut stat = VfsStat::default();
        desc.file_description().stat(&mut stat).map_err(|_| EIO)?;
        Ok(Stat {
            size: stat.size.into_u64(),
        })
    }

    fn position(&self, fd: Self::Fd) -> Result<u64, Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        Ok(desc.file_description().position().load(Relaxed))
    }

    fn set_position(&self, fd: Self::Fd, position: u64) -> Result<(), Errno> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(EBADF)?;
        desc.file_description().position().store(position, Relaxed);
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
        // Clone the node so the region keeps the device file open for its
        // whole lifetime. OpenFileDescription derefs to VfsNode.
        let node = {
            let fds = self.process.file_descriptors();
            let guard = fds.read();
            let desc = guard.get(&fd).ok_or(EBADF)?;
            VfsNode::clone(desc.file_description())
        };

        let region = node.mmap().map_err(|e| match e {
            MmapError::NotSupported => ENODEV,
            MmapError::FsError(_) => EINVAL,
        })?;

        let page_size = Size4KiB::SIZE as usize;
        let page_aligned = len.next_multiple_of(page_size);
        if page_aligned > region.len.next_multiple_of(page_size) {
            return Err(EINVAL);
        }

        // The mmap pointer is a kernel HHDM virtual address. Translate it to
        // the physical base of the contiguous device frames.
        let phys = AddressSpace::kernel()
            .translate(VirtAddr::from_ptr(region.ptr.as_ptr()))
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

        // The region owns the virtual reservation and the open device file,
        // but not the physical frames, so process exit never frees them.
        let inner =
            MemoryRegion::Shared(SharedMemoryRegion::new(segment, page_aligned, frames, node));
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
            VfsNode::clone(desc.file_description())
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
