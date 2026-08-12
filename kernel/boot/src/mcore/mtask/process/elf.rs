use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_elfloader::{
    ElfFile, ElfHeader, ElfParseError, ElfType, ProgramHeaderFlags, ProgramHeaderType,
};
use kernel_memapi::{Guarded, Location, MemoryApi, UserAccessible};
use kernel_vfs::Stat;
use kernel_vfs::node::VfsNode;
use kernel_virtual_memory::Segment;
use thiserror::Error;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::FsBase;
use x86_64::structures::paging::{PageSize, PageTableFlags, Size4KiB};

use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::process::mem::{FileBackedMemoryRegion, LazyMemoryRegion, MemoryRegion};
use crate::mcore::mtask::task::Task;
use crate::mem::memapi::LowerHalfMemoryApi;
use crate::mem::virt::VirtualMemoryAllocator;
use crate::{U64Ext, UsizeExt};

#[derive(Debug, Error)]
pub enum LoadExecutableError {
    #[error("failed to stat the executable")]
    Stat,
    #[error("failed to read the executable")]
    Read,
    #[error("the executable is truncated")]
    Truncated,
    #[error("{0}")]
    Parse(#[from] ElfParseError),
    #[error("unsupported elf type {0:?}")]
    UnsupportedType(ElfType),
    #[error("segment at vaddr {vaddr:#x} is not page congruent with file offset {offset:#x}")]
    NotPageCongruent { vaddr: usize, offset: usize },
    #[error("segment at vaddr {0:#x} is both writable and executable")]
    WritableExecutable(usize),
    #[error("invalid virtual address {0:#x}")]
    InvalidVirtualAddress(usize),
    #[error("size or alignment requirement is invalid")]
    InvalidSizeOrAlign,
    #[error(
        "segment at vaddr {vaddr:#x} claims {filesz:#x} file bytes in {memsz:#x} bytes of memory"
    )]
    FileLongerThanMemory {
        vaddr: usize,
        filesz: usize,
        memsz: usize,
    },
    #[error("virtual range at {0:p} is already reserved")]
    AlreadyReserved(VirtAddr),
    #[error("could not allocate memory")]
    AllocationFailed,
    #[error("more than one TLS header found")]
    TooManyTlsHeaders,
}

/// An executable that passed every check that can fail with a user-visible
/// error.
pub struct ValidatedExecutable {
    header: Vec<u8>,
}

/// Runs every static check on the executable without touching the process.
pub fn validate(node: &VfsNode) -> Result<ValidatedExecutable, LoadExecutableError> {
    let stat = {
        let mut stat = Stat::default();
        node.stat(&mut stat)
            .map_err(|_| LoadExecutableError::Stat)?;
        stat
    };

    let phte = {
        let mut header_buf = [0u8; size_of::<ElfHeader>()];
        read_exact(node, &mut header_buf, 0)?;
        ElfFile::try_parse(&header_buf)?.program_header_table_end()
    };

    if phte > stat.size {
        return Err(LoadExecutableError::Truncated);
    }

    let mut buf = vec![0u8; phte];
    read_exact(node, &mut buf, 0)?;
    {
        let elf = ElfFile::try_parse(&buf)?;

        if *elf.typ() != ElfType::Exec {
            return Err(LoadExecutableError::UnsupportedType(elf.typ().clone()));
        }

        let page_size = Size4KiB::SIZE.into_usize();
        for hdr in elf.program_headers_by_type(ProgramHeaderType::LOAD) {
            if hdr.filesz > hdr.memsz {
                return Err(LoadExecutableError::FileLongerThanMemory {
                    vaddr: hdr.vaddr,
                    filesz: hdr.filesz,
                    memsz: hdr.memsz,
                });
            }
            if hdr.memsz == 0 {
                continue;
            }

            let executable = hdr.flags.contains(&ProgramHeaderFlags::EXECUTABLE);
            let writable = hdr.flags.contains(&ProgramHeaderFlags::WRITABLE);
            if executable && writable {
                return Err(LoadExecutableError::WritableExecutable(hdr.vaddr));
            }

            if hdr.offset % page_size != hdr.vaddr % page_size {
                return Err(LoadExecutableError::NotPageCongruent {
                    vaddr: hdr.vaddr,
                    offset: hdr.offset,
                });
            }

            VirtAddr::try_new(hdr.vaddr as u64)
                .map_err(|_| LoadExecutableError::InvalidVirtualAddress(hdr.vaddr))?;
            if writable {
                Layout::from_size_align(hdr.memsz, hdr.align)
                    .map_err(|_| LoadExecutableError::InvalidSizeOrAlign)?;
            }
        }

        let mut tls_headers = elf.program_headers_by_type(ProgramHeaderType::TLS);
        if let Some(tls) = tls_headers.next() {
            if tls_headers.next().is_some() {
                return Err(LoadExecutableError::TooManyTlsHeaders);
            }
            if tls.filesz > tls.memsz {
                return Err(LoadExecutableError::FileLongerThanMemory {
                    vaddr: tls.vaddr,
                    filesz: tls.filesz,
                    memsz: tls.memsz,
                });
            }
            Layout::from_size_align(tls.memsz, tls.align)
                .map_err(|_| LoadExecutableError::InvalidSizeOrAlign)?;
        }
    }

    Ok(ValidatedExecutable { header: buf })
}

impl ValidatedExecutable {
    /// Commits the executable into the process. Failures past this point
    /// leave the process partially populated, so execve callers must not
    /// return to the old image after calling this.
    pub fn load(
        &self,
        process: &Arc<Process>,
        task: &Task,
        node: &VfsNode,
    ) -> Result<usize, LoadExecutableError> {
        let elf = ElfFile::try_parse(&self.header)?;

        let mut memapi = LowerHalfMemoryApi::new(process.clone());

        for hdr in elf.program_headers_by_type(ProgramHeaderType::LOAD) {
            if hdr.memsz == 0 {
                continue;
            }

            let executable = hdr.flags.contains(&ProgramHeaderFlags::EXECUTABLE);
            let writable = hdr.flags.contains(&ProgramHeaderFlags::WRITABLE);

            let vaddr = VirtAddr::try_new(hdr.vaddr as u64)
                .map_err(|_| LoadExecutableError::InvalidVirtualAddress(hdr.vaddr))?;
            let seg_start = vaddr.align_down(Size4KiB::SIZE);
            let seg_end = (vaddr + hdr.memsz.into_u64()).align_up(Size4KiB::SIZE);

            if writable {
                let layout = Layout::from_size_align(hdr.memsz, hdr.align)
                    .map_err(|_| LoadExecutableError::InvalidSizeOrAlign)?;
                let mut alloc = memapi
                    .allocate(
                        Location::Fixed(vaddr),
                        layout,
                        UserAccessible::Yes,
                        Guarded::No,
                    )
                    .ok_or(LoadExecutableError::AllocationFailed)?;
                let slice = alloc.as_mut();
                read_exact(node, &mut slice[..hdr.filesz], hdr.offset)?;
                slice[hdr.filesz..].fill(0);
                process.executable_segments().write().push(alloc);
            } else {
                let segment = Segment::new(seg_start, seg_end - seg_start);
                let owned = process
                    .vmm()
                    .mark_as_reserved(segment)
                    .map_err(|_| LoadExecutableError::AlreadyReserved(seg_start))?;
                let flags = PageTableFlags::PRESENT
                    | PageTableFlags::USER_ACCESSIBLE
                    | if executable {
                        PageTableFlags::empty()
                    } else {
                        PageTableFlags::NO_EXECUTE
                    };
                let lead = hdr.vaddr - seg_start.as_u64().into_usize();
                let lazy = LazyMemoryRegion::new(owned, (seg_end - seg_start).into_usize(), flags);
                process
                    .memory_regions()
                    .add_region(MemoryRegion::FileBacked(FileBackedMemoryRegion::new(
                        lazy,
                        node.clone(),
                        hdr.offset - lead,
                        lead + hdr.filesz,
                    )));
            }
        }

        let tls = elf.program_headers_by_type(ProgramHeaderType::TLS).next();
        if let Some(tls) = tls {
            let layout = Layout::from_size_align(tls.memsz, tls.align)
                .map_err(|_| LoadExecutableError::InvalidSizeOrAlign)?;
            let mut alloc = memapi
                .allocate(Location::Anywhere, layout, UserAccessible::Yes, Guarded::No)
                .ok_or(LoadExecutableError::AllocationFailed)?;
            let slice = alloc.as_mut();
            read_exact(node, &mut slice[..tls.filesz], tls.offset)?;
            slice[tls.filesz..].fill(0);
            FsBase::write(alloc.start());
            *task.tls().write() = Some(alloc);
        }

        Ok(elf.entry())
    }
}

fn read_exact(node: &VfsNode, buf: &mut [u8], offset: usize) -> Result<(), LoadExecutableError> {
    let mut done = 0;
    while done < buf.len() {
        let n = node
            .read(&mut buf[done..], offset + done)
            .map_err(|_| LoadExecutableError::Read)?;
        if n == 0 {
            return Err(LoadExecutableError::Truncated);
        }
        done += n;
    }
    Ok(())
}
