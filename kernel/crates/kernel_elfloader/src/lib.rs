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
            let pdata = image.elf_file.program_data(hdr);

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

        let pdata = image.elf_file.program_data(tls);

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

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::vec;
    use core::cell::RefCell;

    use kernel_memapi::{Allocation, WritableAllocation};

    use super::*;

    // Mock allocation types for testing
    #[derive(Debug, Clone)]
    struct MockAllocation {
        data: Vec<u8>,
        layout: Layout,
        location: VirtAddr,
    }

    impl Allocation for MockAllocation {
        fn layout(&self) -> Layout {
            self.layout
        }
    }

    impl AsRef<[u8]> for MockAllocation {
        fn as_ref(&self) -> &[u8] {
            &self.data
        }
    }

    impl AsMut<[u8]> for MockAllocation {
        fn as_mut(&mut self) -> &mut [u8] {
            &mut self.data
        }
    }

    impl WritableAllocation for MockAllocation {}

    // Mock memory API for testing
    struct MockMemoryApi {
        allocations: RefCell<BTreeMap<u64, MockAllocation>>,
        should_fail_allocation: bool,
        should_fail_make_executable: bool,
        should_fail_make_readonly: bool,
    }

    impl MockMemoryApi {
        fn new() -> Self {
            Self {
                allocations: RefCell::new(BTreeMap::new()),
                should_fail_allocation: false,
                should_fail_make_executable: false,
                should_fail_make_readonly: false,
            }
        }

        fn with_failing_allocation() -> Self {
            Self {
                allocations: RefCell::new(BTreeMap::new()),
                should_fail_allocation: true,
                should_fail_make_executable: false,
                should_fail_make_readonly: false,
            }
        }
    }

    impl MemoryApi for MockMemoryApi {
        type ReadonlyAllocation = MockAllocation;
        type WritableAllocation = MockAllocation;
        type ExecutableAllocation = MockAllocation;

        fn allocate(
            &mut self,
            location: Location,
            layout: Layout,
            _user_accessible: UserAccessible,
            _guarded: Guarded,
        ) -> Option<Self::WritableAllocation> {
            if self.should_fail_allocation {
                return None;
            }

            let addr = match location {
                Location::Fixed(addr) => addr,
                Location::Anywhere => {
                    // Find a free spot starting from 0x1000
                    let mut candidate = 0x1000u64;
                    let allocations = self.allocations.borrow();
                    while allocations.contains_key(&candidate) {
                        candidate += 0x1000;
                    }
                    VirtAddr::new(candidate)
                }
            };

            let mut data = vec![0u8; layout.size()];
            // Initialize with a pattern to detect uninitialized memory
            data.fill(0xCC);

            let alloc = MockAllocation {
                data,
                layout,
                location: addr,
            };

            self.allocations
                .borrow_mut()
                .insert(addr.as_u64(), alloc.clone());
            Some(alloc)
        }

        fn make_executable(
            &mut self,
            allocation: Self::WritableAllocation,
        ) -> Result<Self::ExecutableAllocation, Self::WritableAllocation> {
            if self.should_fail_make_executable {
                return Err(allocation);
            }
            Ok(allocation)
        }

        fn make_writable(
            &mut self,
            allocation: Self::ExecutableAllocation,
        ) -> Result<Self::WritableAllocation, Self::ExecutableAllocation> {
            Ok(allocation)
        }

        fn make_readonly(
            &mut self,
            allocation: Self::WritableAllocation,
        ) -> Result<Self::ReadonlyAllocation, Self::WritableAllocation> {
            if self.should_fail_make_readonly {
                return Err(allocation);
            }
            Ok(allocation)
        }
    }

    // Helper to create minimal valid ELF header
    fn create_minimal_elf_header() -> [u8; 64] {
        let mut data = [0u8; 64];
        // ELF magic
        data[0..4].copy_from_slice(&[0x7f, 0x45, 0x4c, 0x46]);
        // 64-bit
        data[4] = 2;
        // little-endian
        data[5] = 1;
        // ELF version
        data[6] = 1;
        // OS ABI (System V)
        data[7] = 0;
        // ET_EXEC
        data[16..18].copy_from_slice(&2u16.to_le_bytes());
        // version
        data[20..24].copy_from_slice(&1u32.to_le_bytes());
        // shoff = 0 (no section headers)
        data[40..48].copy_from_slice(&0usize.to_le_bytes());
        // ehsize
        data[52..54].copy_from_slice(&64u16.to_le_bytes());
        // phentsize
        data[54..56].copy_from_slice(&56u16.to_le_bytes());
        data[56..58].copy_from_slice(&0u16.to_le_bytes()); // phnum = 0
        data[58..60].copy_from_slice(&64u16.to_le_bytes()); // shentsize
        data[60..62].copy_from_slice(&0u16.to_le_bytes()); // shnum = 0
        data[62..64].copy_from_slice(&0u16.to_le_bytes()); // shstrndx = 0
        data
    }

    #[test]
    fn test_elf_loader_new() {
        let memory_api = MockMemoryApi::new();
        let _loader = ElfLoader::new(memory_api);
    }

    #[test]
    fn test_load_minimal_elf() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        let header_data = create_minimal_elf_header();
        let elf_file = ElfFile::try_parse(&header_data).unwrap();

        let result = loader.load(elf_file);
        assert!(result.is_ok());
        let image = result.unwrap();
        assert_eq!(image.executable_allocations().len(), 0);
        assert_eq!(image.readonly_allocations().len(), 0);
        assert_eq!(image.writable_allocations().len(), 0);
        assert!(image.tls_allocation().is_none());
    }

    #[test]
    fn test_load_elf_with_load_segment() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        // Create ELF with program header
        let mut data = vec![0u8; 64 + 56]; // header + 1 program header
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        // Set phoff to point after header
        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        // Set phnum to 1
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Program header: PT_LOAD
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&5u32.to_le_bytes()); // R+X flags
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x1000usize.to_le_bytes()); // vaddr
        data[ph_offset + 24..ph_offset + 32].copy_from_slice(&0x1000usize.to_le_bytes()); // paddr
        data[ph_offset + 32..ph_offset + 40].copy_from_slice(&0usize.to_le_bytes()); // filesz
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes()); // memsz
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes()); // align

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(result.is_ok());
        let image = result.unwrap();
        assert_eq!(image.executable_allocations().len(), 1);
        assert_eq!(image.readonly_allocations().len(), 0);
        assert_eq!(image.writable_allocations().len(), 0);
    }

    #[test]
    fn test_load_elf_with_writable_segment() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        let mut data = vec![0u8; 64 + 56];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Program header: PT_LOAD with write flag
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&6u32.to_le_bytes()); // R+W flags
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x2000usize.to_le_bytes()); // vaddr
        data[ph_offset + 24..ph_offset + 32].copy_from_slice(&0x2000usize.to_le_bytes()); // paddr
        data[ph_offset + 32..ph_offset + 40].copy_from_slice(&0usize.to_le_bytes()); // filesz
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes()); // memsz
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes()); // align

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(result.is_ok());
        let image = result.unwrap();
        assert_eq!(image.executable_allocations().len(), 0);
        assert_eq!(image.readonly_allocations().len(), 0);
        assert_eq!(image.writable_allocations().len(), 1);
    }

    #[test]
    fn test_load_elf_with_readonly_segment() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        let mut data = vec![0u8; 64 + 56];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Program header: PT_LOAD with read-only
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&4u32.to_le_bytes()); // R only
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x3000usize.to_le_bytes()); // vaddr
        data[ph_offset + 24..ph_offset + 32].copy_from_slice(&0x3000usize.to_le_bytes()); // paddr
        data[ph_offset + 32..ph_offset + 40].copy_from_slice(&0usize.to_le_bytes()); // filesz
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes()); // memsz
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes()); // align

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(result.is_ok());
        let image = result.unwrap();
        assert_eq!(image.executable_allocations().len(), 0);
        assert_eq!(image.readonly_allocations().len(), 1);
        assert_eq!(image.writable_allocations().len(), 0);
    }

    #[test]
    fn test_load_elf_allocation_failure() {
        let mut memory_api = MockMemoryApi::with_failing_allocation();
        let mut loader = ElfLoader::new(memory_api);

        let mut data = vec![0u8; 64 + 56];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&5u32.to_le_bytes()); // R+X
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x1000usize.to_le_bytes()); // vaddr
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes()); // memsz
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes()); // align

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(matches!(result, Err(LoadElfError::AllocationFailed)));
    }

    #[test]
    fn test_load_elf_with_tls_segment() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        let mut data = vec![0u8; 64 + 56];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Program header: PT_TLS
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&7u32.to_le_bytes()); // PT_TLS
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&4u32.to_le_bytes()); // R
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x4000usize.to_le_bytes()); // vaddr
        data[ph_offset + 32..ph_offset + 40].copy_from_slice(&0usize.to_le_bytes()); // filesz
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes()); // memsz
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&8usize.to_le_bytes()); // align

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(result.is_ok());
        let image = result.unwrap();
        assert!(image.tls_allocation().is_some());
    }

    #[test]
    fn test_load_elf_data_copied_correctly() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        // Create ELF with actual data in segment
        let segment_data = b"Hello, World!";
        let mut data = vec![0u8; 64 + 56 + segment_data.len()];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Place segment data after headers
        let segment_offset = 64 + 56;
        data[segment_offset..segment_offset + segment_data.len()].copy_from_slice(segment_data);

        // Program header: PT_LOAD
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&6u32.to_le_bytes()); // R+W
        data[ph_offset + 8..ph_offset + 16].copy_from_slice(&segment_offset.to_le_bytes()); // offset
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x5000usize.to_le_bytes()); // vaddr
        data[ph_offset + 32..ph_offset + 40].copy_from_slice(&segment_data.len().to_le_bytes()); // filesz
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes()); // memsz (larger than filesz)
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes()); // align

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(result.is_ok());
        let image = result.unwrap();
        assert_eq!(image.writable_allocations().len(), 1);

        // Verify data was copied correctly
        let alloc = &image.writable_allocations()[0];
        let alloc_data = alloc.as_ref();
        assert_eq!(&alloc_data[..segment_data.len()], segment_data);
        // Verify zero-padding
        assert_eq!(alloc_data[segment_data.len()], 0);
    }

    #[test]
    fn test_load_elf_invalid_size_or_align() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        let mut data = vec![0u8; 64 + 56];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Program header with invalid alignment (not power of 2)
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&5u32.to_le_bytes()); // R+X
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x1000usize.to_le_bytes()); // vaddr
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes()); // memsz
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&3usize.to_le_bytes()); // invalid align

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(matches!(result, Err(LoadElfError::InvalidSizeOrAlign)));
    }

    #[test]
    fn test_load_elf_multiple_segments() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        // Create ELF with 3 program headers: executable, writable, readonly
        let mut data = vec![0u8; 64 + 56 * 3];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&3u16.to_le_bytes()); // 3 program headers

        // First segment: executable
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes());
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&5u32.to_le_bytes()); // R+X
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x1000usize.to_le_bytes());
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes());
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes());

        // Second segment: writable
        let ph_offset = 64 + 56;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes());
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&6u32.to_le_bytes()); // R+W
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x2000usize.to_le_bytes());
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes());
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes());

        // Third segment: readonly
        let ph_offset = 64 + 56 * 2;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes());
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&4u32.to_le_bytes()); // R
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x3000usize.to_le_bytes());
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes());
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes());

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(result.is_ok());
        let image = result.unwrap();
        assert_eq!(image.executable_allocations().len(), 1);
        assert_eq!(image.writable_allocations().len(), 1);
        assert_eq!(image.readonly_allocations().len(), 1);
    }

    #[test]
    #[should_panic(expected = "segments that are executable and writable are not supported")]
    fn test_load_elf_executable_and_writable_segment_panics() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        let mut data = vec![0u8; 64 + 56];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Program header with both executable and writable flags
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&1u32.to_le_bytes());
        data[ph_offset + 4..ph_offset + 8].copy_from_slice(&7u32.to_le_bytes()); // R+W+X (invalid)
        data[ph_offset + 16..ph_offset + 24].copy_from_slice(&0x1000usize.to_le_bytes());
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes());
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&0x1000usize.to_le_bytes());

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let _ = loader.load(elf_file);
    }

    #[test]
    fn test_load_elf_multiple_tls_segments_error() {
        let mut memory_api = MockMemoryApi::new();
        let mut loader = ElfLoader::new(memory_api);

        let mut data = vec![0u8; 64 + 56 * 2];
        let header_data = create_minimal_elf_header();
        data[..64].copy_from_slice(&header_data);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&2u16.to_le_bytes());

        // First TLS segment
        let ph_offset = 64;
        data[ph_offset..ph_offset + 4].copy_from_slice(&7u32.to_le_bytes()); // PT_TLS
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes());
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&8usize.to_le_bytes());

        // Second TLS segment (invalid)
        let ph_offset = 64 + 56;
        data[ph_offset..ph_offset + 4].copy_from_slice(&7u32.to_le_bytes()); // PT_TLS
        data[ph_offset + 40..ph_offset + 48].copy_from_slice(&0x100usize.to_le_bytes());
        data[ph_offset + 48..ph_offset + 56].copy_from_slice(&8usize.to_le_bytes());

        let elf_file = ElfFile::try_parse(&data).unwrap();
        let result = loader.load(elf_file);
        assert!(matches!(result, Err(LoadElfError::TooManyTlsHeaders)));
    }
}
