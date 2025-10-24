#![no_std]
extern crate alloc;

mod file;

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::fmt::Debug;

pub use file::*;
use itertools::Itertools;
use kernel_memapi::{Guarded, Location, MemoryApi, UserAccessible};
use log::trace;
use thiserror::Error;
use x86_64::VirtAddr;
use x86_64::addr::VirtAddrNotValid;

pub struct ElfLoader<M>
where
    M: MemoryApi,
{
    memory_api: M,
}

#[derive(Debug, Eq, PartialEq, Error)]
pub enum LoadElfError {
    #[error("could not allocate memory")]
    AllocationFailed,
    #[error("unsupported file type")]
    UnsupportedFileType(ElfType),
    #[error("size or alignment requirement is invalid")]
    InvalidSizeOrAlign,
    #[error("invalid virtual address 0x{0:016x}")]
    InvalidVirtualAddress(usize),
    #[error("more than one TLS header found")]
    TooManyTlsHeaders,
}

impl From<VirtAddrNotValid> for LoadElfError {
    fn from(value: VirtAddrNotValid) -> Self {
        Self::InvalidVirtualAddress(usize::try_from(value.0).unwrap())
    }
}

impl<M> ElfLoader<M>
where
    M: MemoryApi,
{
    pub fn new(memory_api: M) -> Self {
        Self { memory_api }
    }

    /// # Errors
    /// Returns an error if the ELF file is not supported or if a required memory allocation fails.
    ///
    /// # Panics
    /// Panics if the ELF file is not of type `ET_EXEC`.
    pub fn load<'a>(&mut self, elf_file: ElfFile<'a>) -> Result<ElfImage<'a, M>, LoadElfError>
    where
        <M as MemoryApi>::WritableAllocation: Debug,
    {
        assert_eq!(
            ElfType::Exec,
            elf_file.header.typ,
            "only ET_EXEC supported for now"
        );

        let mut image = ElfImage {
            elf_file,
            executable_allocations: vec![],
            readonly_allocations: vec![],
            writable_allocations: vec![],
            tls_allocation: None,
        };

        self.load_loadable_headers(&mut image)?;
        self.load_tls(&mut image)?;

        Ok(image)
    }

    fn load_loadable_headers(&mut self, image: &mut ElfImage<'_, M>) -> Result<(), LoadElfError> {
        for hdr in image
            .elf_file
            .program_headers_by_type(ProgramHeaderType::LOAD)
        {
            trace!("load header {hdr:x?}");
            let pdata = image.elf_file.program_data(&hdr);

            let location = Location::Fixed(VirtAddr::try_new(hdr.vaddr as u64)?);

            let layout = Layout::from_size_align(hdr.memsz, hdr.align)
                .map_err(|_| LoadElfError::InvalidSizeOrAlign)?;

            let mut alloc = self
                .memory_api
                .allocate(location, layout, UserAccessible::Yes, Guarded::No) // TODO: make user accessibility configurable
                .ok_or(LoadElfError::AllocationFailed)?;

            let slice = alloc.as_mut();
            slice[..hdr.filesz].copy_from_slice(pdata);
            slice[hdr.filesz..].fill(0);

            assert!(
                !(hdr.flags.contains(&ProgramHeaderFlags::EXECUTABLE)
                    && hdr.flags.contains(&ProgramHeaderFlags::WRITABLE)),
                "segments that are executable and writable are not supported"
            );
            if hdr.flags.contains(&ProgramHeaderFlags::EXECUTABLE) {
                let alloc = self
                    .memory_api
                    .make_executable(alloc)
                    .map_err(|_| LoadElfError::AllocationFailed)?;
                image.executable_allocations.push(alloc);
            } else if hdr.flags.contains(&ProgramHeaderFlags::WRITABLE) {
                image.writable_allocations.push(alloc);
            } else {
                let alloc = self
                    .memory_api
                    .make_readonly(alloc)
                    .map_err(|_| LoadElfError::AllocationFailed)?;
                image.readonly_allocations.push(alloc);
            }
        }
        Ok(())
    }

    fn load_tls(&mut self, image: &mut ElfImage<'_, M>) -> Result<(), LoadElfError> {
        let Some(tls) = image
            .elf_file
            .program_headers_by_type(ProgramHeaderType::TLS)
            .at_most_one()
            .map_err(|_| LoadElfError::TooManyTlsHeaders)?
        else {
            return Ok(());
        };
        trace!("tls header {tls:x?}");

        let pdata = image.elf_file.program_data(&tls);

        let layout = Layout::from_size_align(tls.memsz, tls.align)
            .map_err(|_| LoadElfError::InvalidSizeOrAlign)?;

        let mut alloc = self
            .memory_api
            .allocate(Location::Anywhere, layout, UserAccessible::Yes, Guarded::No) // TODO: make user accessibility configurable
            .ok_or(LoadElfError::AllocationFailed)?;

        let slice = alloc.as_mut();
        slice[..tls.filesz].copy_from_slice(pdata);
        slice[tls.filesz..].fill(0);

        let alloc = self
            .memory_api
            .make_readonly(alloc)
            .map_err(|_| LoadElfError::AllocationFailed)?;

        image.tls_allocation = Some(alloc);

        Ok(())
    }
}

pub struct ElfImage<'a, M>
where
    M: MemoryApi,
{
    elf_file: ElfFile<'a>,
    executable_allocations: Vec<M::ExecutableAllocation>,
    readonly_allocations: Vec<M::ReadonlyAllocation>,
    writable_allocations: Vec<M::WritableAllocation>,
    tls_allocation: Option<M::ReadonlyAllocation>,
}

impl<M> ElfImage<'_, M>
where
    M: MemoryApi,
{
    pub fn entry(&self) -> usize {
        self.elf_file.entry()
    }

    pub fn executable_allocations(&self) -> &[M::ExecutableAllocation] {
        &self.executable_allocations
    }

    pub fn readonly_allocations(&self) -> &[M::ReadonlyAllocation] {
        &self.readonly_allocations
    }

    pub fn writable_allocations(&self) -> &[M::WritableAllocation] {
        &self.writable_allocations
    }

    pub fn tls_allocation(&self) -> Option<&M::ReadonlyAllocation> {
        self.tls_allocation.as_ref()
    }
}
