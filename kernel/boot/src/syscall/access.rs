use alloc::sync::Arc;
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::{EBADF, EINVAL, EIO, ENODEV, ENOMEM, ENOTTY, Errno, IoctlRequest, ProtFlags, Stat};
use kernel_syscall::access::{CwdAccess, FileAccess};
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{FsyncError, IoctlError, MmapError};
use kernel_vfs::Stat as VfsStat;
use spin::rwlock::RwLock;
use x86_64::VirtAddr;
use x86_64::structures::paging::{PageSize, PageTableFlags, PhysFrame, Size4KiB};

use crate::{U64Ext, UsizeExt};
use crate::file::{OpenFileDescription, vfs};
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::process::fd::{FdNum, FileDescriptor, FileDescriptorFlags};
use crate::mcore::mtask::process::mem::{
    FileBackedMemoryRegion, LazyMemoryRegion, MemoryRegion, SharedMemoryRegion,
};
use crate::mcore::mtask::task::Task;
use crate::mem::address_space::AddressSpace;
use crate::mem::virt::VirtualMemoryAllocator;

mod mem;
mod signal;

pub struct KernelAccess<'a> {
    _task: &'a Task,
    process: Arc<Process>,
}

impl<'a> KernelAccess<'a> {
    pub fn new() -> Self {
        let task = ExecutionContext::load().current_task();
        let process = task.process().clone(); // TODO: can we remove the clone?

        KernelAccess {
            _task: task,
            process,
        }
    }
}

impl CwdAccess for KernelAccess<'_> {
    fn current_working_directory(&self) -> &RwLock<kernel_vfs::path::AbsoluteOwnedPath> {
        self.process.current_working_directory()
    }
}

pub struct FileInfo {
    node: VfsNode,
}

impl kernel_syscall::access::FileInfo for FileInfo {}

impl FileAccess for KernelAccess<'_> {
    type FileInfo = FileInfo;
    type Fd = FdNum;
    type OpenError = ();
    type ReadError = ();
    type WriteError = ();
    type CloseError = ();

    fn file_info(&self, path: &AbsolutePath) -> Option<Self::FileInfo> {
        Some(FileInfo {
            node: vfs().read().open(path).ok()?,
        })
    }

    fn open(&self, info: &Self::FileInfo) -> Result<Self::Fd, ()> {
        let ofd = OpenFileDescription::from(info.node.clone());
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

        Ok(num)
    }

    fn read(&self, fd: Self::Fd, buf: &mut [u8]) -> Result<usize, ()> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(())?;
        let ofd = desc.file_description();
        let offset = ofd.position().load(Relaxed);
        let read = ofd.read(buf, offset.into_usize()).map_err(|_| ())?;
        // The load and the store are separate, which is sound only while a
        // single task reaches a file description. Nothing forks and no
        // thread-spawn syscall is dispatched.
        ofd.position().store(offset + read.into_u64(), Relaxed);
        Ok(read)
    }

    fn write(&self, fd: Self::Fd, buf: &[u8]) -> Result<usize, ()> {
        let fds = self.process.file_descriptors();
        let guard = fds.read();

        let desc = guard.get(&fd).ok_or(())?;
        let ofd = desc.file_description();
        let offset = ofd.position().load(Relaxed);
        let written = ofd.write(buf, offset.into_usize()).map_err(|_| ())?;
        ofd.position().store(offset + written.into_u64(), Relaxed);
        Ok(written)
    }

    fn close(&self, fd: Self::Fd) -> Result<(), ()> {
        self.process.file_descriptors().write().remove(&fd);
        Ok(())
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

impl kernel_syscall::access::MemoryRegionAccess for KernelAccess<'_> {
    type Region = KernelMemoryRegionHandle;

    fn create_and_track_mapping(
        &self,
        location: kernel_syscall::access::Location,
        size: usize,
        allocation_strategy: kernel_syscall::access::AllocationStrategy,
    ) -> Result<kernel_syscall::UserspacePtr<u8>, kernel_syscall::access::CreateMappingError> {
        // Use the MemoryAccess trait to create the mapping
        let mapping = <Self as kernel_syscall::access::MemoryAccess>::create_mapping(
            self,
            location,
            size,
            allocation_strategy,
        )?;

        let addr =
            <crate::syscall::access::mem::KernelMapping as kernel_syscall::access::Mapping>::addr(
                &mapping,
            );

        // Convert the mapping to a region and track it
        let region_handle = mapping.into_region_handle();
        self.add_memory_region(region_handle);

        Ok(addr)
    }

    fn add_memory_region(&self, region: Self::Region) {
        self.process.memory_regions().add_region(region.inner);
    }

    fn map_shared_file(
        &self,
        fd: i32,
        len: usize,
    ) -> Result<kernel_syscall::UserspacePtr<u8>, Errno> {
        let fd = FdNum::from(fd);

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
        // the device region must be large enough to satisfy the request
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
        let frames = (0..page_count as u64).map(move |i| start + i);

        let segment = self.process.vmm().reserve(page_count).ok_or(ENOMEM)?;
        let addr = segment.start;

        self.process
            .address_space()
            .map_range::<Size4KiB>(
                &*segment,
                frames,
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::NO_EXECUTE,
            )
            .map_err(|_| EINVAL)?;

        let user_ptr = addr.as_ptr::<u8>().try_into().map_err(|_| EINVAL)?;

        // The region owns the virtual reservation and the open device file,
        // but not the physical frames, so process exit never frees them.
        let inner = MemoryRegion::Shared(SharedMemoryRegion::new(segment, page_aligned, node));
        self.add_memory_region(KernelMemoryRegionHandle {
            addr: user_ptr,
            size: page_aligned,
            inner,
        });

        Ok(user_ptr)
    }

    fn map_private_file(
        &self,
        fd: i32,
        len: usize,
        offset: usize,
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
            let desc = guard.get(&FdNum::from(fd)).ok_or(EBADF)?;
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

        let flags = PageTableFlags::PRESENT
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
            };

        let user_ptr = addr.as_ptr::<u8>().try_into().map_err(|_| EINVAL)?;
        // The region must carry the page-rounded size. `MemoryRegion::contains`
        // bounds the fault handler, so a region sized to `len` leaves the last
        // partial page unservable and any access to it kills the process.
        let lazy = LazyMemoryRegion::new(segment, size, flags);
        self.add_memory_region(KernelMemoryRegionHandle {
            addr: user_ptr,
            size,
            inner: MemoryRegion::FileBacked(FileBackedMemoryRegion::new(
                lazy, node, offset, file_len,
            )),
        });

        Ok(user_ptr)
    }
}

/// A handle to a memory region that implements the MemoryRegion trait
/// from kernel_syscall. This bridges the gap between the syscall layer
/// and the kernel's internal MemoryRegion type.
pub struct KernelMemoryRegionHandle {
    addr: kernel_syscall::UserspacePtr<u8>,
    size: usize,
    inner: crate::mcore::mtask::process::mem::MemoryRegion,
}

impl kernel_syscall::access::MemoryRegion for KernelMemoryRegionHandle {
    fn addr(&self) -> kernel_syscall::UserspacePtr<u8> {
        self.addr
    }

    fn size(&self) -> usize {
        self.size
    }
}
