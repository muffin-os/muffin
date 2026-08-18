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
    entry: usize,
    segments: Vec<ValidatedSegment>,
    tls: Option<ValidatedTls>,
}

enum ValidatedSegment {
    Private {
        vaddr: VirtAddr,
        layout: Layout,
        offset: usize,
        filesz: usize,
    },
    Mapped {
        start: VirtAddr,
        len: usize,
        no_execute: bool,
        offset: usize,
        filesz: usize,
    },
}

struct ValidatedTls {
    layout: Layout,
    offset: usize,
    filesz: usize,
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
    let elf = ElfFile::try_parse(&buf)?;

    if *elf.typ() != ElfType::Exec {
        return Err(LoadExecutableError::UnsupportedType(elf.typ().clone()));
    }

    let page_size = Size4KiB::SIZE.into_usize();
    let mut segments = vec![];
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

        let writable = hdr.flags.contains(&ProgramHeaderFlags::WRITABLE);
        let executable = hdr.flags.contains(&ProgramHeaderFlags::EXECUTABLE);
        if executable && writable {
            return Err(LoadExecutableError::WritableExecutable(hdr.vaddr));
        }

        if hdr.offset % page_size != hdr.vaddr % page_size {
            return Err(LoadExecutableError::NotPageCongruent {
                vaddr: hdr.vaddr,
                offset: hdr.offset,
            });
        }

        let vaddr = VirtAddr::try_new(hdr.vaddr.into_u64())
            .map_err(|_| LoadExecutableError::InvalidVirtualAddress(hdr.vaddr))?;

        if writable {
            let layout = Layout::from_size_align(hdr.memsz, hdr.align)
                .map_err(|_| LoadExecutableError::InvalidSizeOrAlign)?;
            segments.push(ValidatedSegment::Private {
                vaddr,
                layout,
                offset: hdr.offset,
                filesz: hdr.filesz,
            });
        } else {
            let start = vaddr.align_down(Size4KiB::SIZE);
            let end = hdr
                .vaddr
                .checked_add(hdr.memsz)
                .and_then(|e| e.checked_next_multiple_of(page_size))
                .ok_or(LoadExecutableError::InvalidVirtualAddress(hdr.vaddr))?;
            VirtAddr::try_new(end.into_u64())
                .map_err(|_| LoadExecutableError::InvalidVirtualAddress(hdr.vaddr))?;
            let start_usize = start.as_u64().into_usize();
            let lead = hdr.vaddr - start_usize;
            segments.push(ValidatedSegment::Mapped {
                start,
                len: end - start_usize,
                no_execute: !executable,
                // Page congruence of offset and vaddr guarantees `hdr.offset >= lead`.
                offset: hdr.offset - lead,
                filesz: lead + hdr.filesz,
            });
        }
    }

    let mut tls_headers = elf.program_headers_by_type(ProgramHeaderType::TLS);
    let tls = if let Some(tls) = tls_headers.next() {
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
        let layout = Layout::from_size_align(tls.memsz, tls.align)
            .map_err(|_| LoadExecutableError::InvalidSizeOrAlign)?;
        Some(ValidatedTls {
            layout,
            offset: tls.offset,
            filesz: tls.filesz,
        })
    } else {
        None
    };

    Ok(ValidatedExecutable {
        entry: elf.entry(),
        segments,
        tls,
    })
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
        let mut memapi = LowerHalfMemoryApi::new(process.clone());

        for segment in &self.segments {
            match *segment {
                ValidatedSegment::Private {
                    vaddr,
                    layout,
                    offset,
                    filesz,
                } => {
                    let mut alloc = memapi
                        .allocate(
                            Location::Fixed(vaddr),
                            layout,
                            UserAccessible::Yes,
                            Guarded::No,
                        )
                        .ok_or(LoadExecutableError::AllocationFailed)?;
                    let slice = alloc.as_mut();
                    read_exact(node, &mut slice[..filesz], offset)?;
                    slice[filesz..].fill(0);
                    process.executable_segments().write().push(alloc);
                }
                ValidatedSegment::Mapped {
                    start,
                    len,
                    no_execute,
                    offset,
                    filesz,
                } => {
                    let segment = Segment::new(start, len.into_u64());
                    let owned = process
                        .vmm()
                        .mark_as_reserved(segment)
                        .map_err(|_| LoadExecutableError::AlreadyReserved(start))?;
                    let flags = PageTableFlags::PRESENT
                        | PageTableFlags::USER_ACCESSIBLE
                        | if no_execute {
                            PageTableFlags::NO_EXECUTE
                        } else {
                            PageTableFlags::empty()
                        };
                    let lazy = LazyMemoryRegion::new(owned, len, flags);
                    process
                        .memory_regions()
                        .add_region(MemoryRegion::FileBacked(FileBackedMemoryRegion::new(
                            lazy,
                            node.clone(),
                            offset,
                            filesz,
                        )));
                }
            }
        }

        if let Some(tls) = &self.tls {
            let mut alloc = memapi
                .allocate(
                    Location::Anywhere,
                    tls.layout,
                    UserAccessible::Yes,
                    Guarded::No,
                )
                .ok_or(LoadExecutableError::AllocationFailed)?;
            let slice = alloc.as_mut();
            read_exact(node, &mut slice[..tls.filesz], tls.offset)?;
            slice[tls.filesz..].fill(0);
            FsBase::write(alloc.start());
            *task.tls().write() = Some(alloc);
        }

        Ok(self.entry)
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
