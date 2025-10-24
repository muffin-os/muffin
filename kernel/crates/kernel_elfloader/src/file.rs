use core::ffi::CStr;
use core::fmt::{Debug, Display, Formatter};

use thiserror::Error;
use zerocopy::{Immutable, KnownLayout, TryFromBytes};

#[derive(Copy, Clone, Debug)]
pub struct ElfFile<'a> {
    pub(crate) source: &'a [u8],
    pub(crate) header: &'a ElfHeader,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum ElfParseError {
    #[error("could not parse elf header")]
    HeaderParseError,
    #[error("invalid magic number")]
    InvalidMagic,
    #[error("invalid e_phentsize")]
    InvalidPhEntSize,
    #[error("invalid e_shentsize")]
    InvalidShEntSize,
    #[error("unsupported os abi")]
    UnsupportedOsAbi,
    #[error("unsupported elf version")]
    UnsupportedElfVersion,
    #[error("unsupported endianness")]
    UnsupportedEndian,
}

impl<'a> ElfFile<'a> {
    /// # Errors
    /// Returns an error if the ELF file is invalid or not supported.
    pub fn try_parse(source: &'a [u8]) -> Result<Self, ElfParseError> {
        #[cfg(target_endian = "little")]
        const ENDIAN: u8 = 1;
        #[cfg(target_endian = "big")]
        const ENDIAN: u8 = 2;

        let header = ElfHeader::try_ref_from_bytes(&source[..size_of::<ElfHeader>()])
            .map_err(|_| ElfParseError::HeaderParseError)?;

        if header.ident.magic != [0x7F, 0x45, 0x4C, 0x46] {
            return Err(ElfParseError::InvalidMagic);
        }

        if header.ident.data != ENDIAN {
            return Err(ElfParseError::UnsupportedEndian);
        }

        if usize::from(header.phentsize) != size_of::<ProgramHeader>() {
            return Err(ElfParseError::InvalidPhEntSize);
        }
        if usize::from(header.shentsize) != size_of::<SectionHeader>() {
            return Err(ElfParseError::InvalidShEntSize);
        }
        if header.ident.version != 1 || header.version != 1 {
            return Err(ElfParseError::UnsupportedElfVersion);
        }
        if header.ident.os_abi != 0x00 {
            // not Sys V
            return Err(ElfParseError::UnsupportedOsAbi);
        }

        Ok(Self { source, header })
    }

    #[must_use]
    pub fn entry(&self) -> usize {
        self.header.entry
    }

    pub fn program_headers(&self) -> impl Iterator<Item = &ProgramHeader> {
        self.headers(self.header.phoff, usize::from(self.header.phnum))
    }

    pub fn program_headers_by_type(
        &self,
        typ: ProgramHeaderType,
    ) -> impl Iterator<Item = &ProgramHeader> {
        self.program_headers().filter(move |h| h.typ == typ)
    }

    pub fn section_headers(&self) -> impl Iterator<Item = &SectionHeader> {
        self.headers(self.header.shoff, usize::from(self.header.shnum))
    }

    pub fn section_headers_by_type(
        &self,
        typ: SectionHeaderType,
    ) -> impl Iterator<Item = &SectionHeader> {
        self.section_headers().filter(move |h| h.typ == typ)
    }

    fn headers<T: TryFromBytes + KnownLayout + Immutable + 'a>(
        &self,
        header_offset: usize,
        header_num: usize,
    ) -> impl Iterator<Item = &T> {
        let size = size_of::<T>();
        let data = &self.source[header_offset..header_offset + (header_num * size)];

        data.chunks_exact(size)
            .map(T::try_ref_from_bytes)
            .map(Result::unwrap)
    }

    #[must_use]
    pub fn section_data(&self, header: &SectionHeader) -> &[u8] {
        &self.source[header.offset..header.offset + header.size]
    }

    #[must_use]
    pub fn section_name(&self, header: &SectionHeader) -> Option<&str> {
        let shstrtab = self
            .section_headers()
            .nth(usize::from(self.header.shstrndx))?;
        let shstrtab_data = self.section_data(shstrtab);
        CStr::from_bytes_until_nul(&shstrtab_data[header.name as usize..])
            .ok()?
            .to_str()
            .ok()
    }

    pub fn sections_by_name(&self, name: &str) -> impl Iterator<Item = &SectionHeader> {
        self.section_headers()
            .filter(move |h| self.section_name(h) == Some(name))
    }

    #[must_use]
    pub fn program_data(&self, header: &ProgramHeader) -> &[u8] {
        &self.source[header.offset..header.offset + header.filesz]
    }

    #[must_use]
    pub fn symtab_data(&'a self, header: &'a SectionHeader) -> SymtabSection<'a> {
        let data = self.section_data(header);
        SymtabSection { header, data }
    }

    #[must_use]
    pub fn symbol_name(&self, symtab: &SymtabSection<'a>, symbol: &Symbol) -> Option<&str> {
        let strtab_index = symtab.header.link as usize;
        let strtab_hdr = self.section_headers().nth(strtab_index)?;
        let strtab_data = self.section_data(strtab_hdr);
        CStr::from_bytes_until_nul(&strtab_data[symbol.name as usize..])
            .ok()
            .and_then(|cstr| cstr.to_str().ok())
    }
}

const _: () = {
    assert!(64 == size_of::<ElfHeader>());
};

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ElfHeader {
    pub ident: ElfIdent,
    pub typ: ElfType,
    pub machine: u16,
    pub version: u32,
    pub entry: usize,
    pub phoff: usize,
    pub shoff: usize,
    pub flags: u32,
    pub ehsize: u16,
    pub phentsize: u16,
    pub phnum: u16,
    pub shentsize: u16,
    pub shnum: u16,
    pub shstrndx: u16,
}

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq, Clone)]
#[repr(u16)]
pub enum ElfType {
    None = 0x00,
    Rel = 0x01,
    Exec = 0x02,
    Dyn = 0x03,
    Core = 0x04,
}

const _: () = {
    assert!(16 == size_of::<ElfIdent>());
};

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ElfIdent {
    pub magic: [u8; 4],
    pub class: u8,
    pub data: u8,
    pub version: u8,
    pub os_abi: u8,
    pub abi_version: u8,
    _padding: [u8; 7],
}

const _: () = {
    assert!(56 == size_of::<ProgramHeader>());
};

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ProgramHeader {
    pub typ: ProgramHeaderType,
    pub flags: ProgramHeaderFlags,
    pub offset: usize,
    pub vaddr: usize,
    pub paddr: usize,
    pub filesz: usize,
    pub memsz: usize,
    pub align: usize,
}

#[derive(TryFromBytes, KnownLayout, Immutable, Eq, PartialEq)]
#[repr(transparent)]
pub struct ProgramHeaderType(pub u16);

impl ProgramHeaderType {
    pub const NULL: Self = Self(0x00);
    pub const LOAD: Self = Self(0x01);
    pub const DYNAMIC: Self = Self(0x02);
    pub const INTERP: Self = Self(0x03);
    pub const NOTE: Self = Self(0x04);
    pub const SHLIB: Self = Self(0x05);
    pub const PHDR: Self = Self(0x06);
    pub const TLS: Self = Self(0x07);
}

impl Debug for ProgramHeaderType {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "ProgramHeaderType({self})")
    }
}

impl Display for ProgramHeaderType {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match *self {
            ProgramHeaderType::NULL => write!(f, "NULL"),
            ProgramHeaderType::LOAD => write!(f, "LOAD"),
            ProgramHeaderType::DYNAMIC => write!(f, "DYNAMIC"),
            ProgramHeaderType::INTERP => write!(f, "INTERP"),
            ProgramHeaderType::NOTE => write!(f, "NOTE"),
            ProgramHeaderType::SHLIB => write!(f, "SHLIB"),
            ProgramHeaderType::PHDR => write!(f, "PHDR"),
            ProgramHeaderType::TLS => write!(f, "TLS"),
            _ => write!(f, "UNKNOWN({})", self.0),
        }
    }
}

#[derive(TryFromBytes, KnownLayout, Immutable, Eq, PartialEq)]
#[repr(transparent)]
pub struct ProgramHeaderFlags(pub u32);

impl ProgramHeaderFlags {
    pub const EXECUTABLE: Self = Self(0x01);
    pub const WRITABLE: Self = Self(0x02);
    pub const READABLE: Self = Self(0x04);
}

impl ProgramHeaderFlags {
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        self.0 & other.0 > 0
    }
}

impl Debug for ProgramHeaderFlags {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "ProgramHeaderFlags({self})")
    }
}

impl Display for ProgramHeaderFlags {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        if self.0 == 0 {
            return write!(f, "NONE");
        }

        let mut first = true;

        if self.contains(&ProgramHeaderFlags::READABLE) {
            write!(f, "R")?;
            first = false;
        }
        if self.contains(&ProgramHeaderFlags::WRITABLE) {
            if !first {
                write!(f, "|")?;
            }
            write!(f, "W")?;
            first = false;
        }
        if self.contains(&ProgramHeaderFlags::EXECUTABLE) {
            if !first {
                write!(f, "|")?;
            }
            write!(f, "X")?;
        }

        Ok(())
    }
}

const _: () = {
    assert!(64 == size_of::<SectionHeader>());
};

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SectionHeader {
    pub name: u32,
    pub typ: SectionHeaderType,
    pub flags: SectionHeaderFlags,
    pub addr: usize,
    pub offset: usize,
    pub size: usize,
    pub link: u32,
    pub info: u32,
    pub addralign: usize,
    pub entsize: usize,
}

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq, Copy, Clone)]
#[repr(transparent)]
pub struct SectionHeaderType(pub u32);

impl SectionHeaderType {
    pub const NULL: Self = Self(0x00);
    pub const PROGBITS: Self = Self(0x01);
    pub const SYMTAB: Self = Self(0x02);
    pub const STRTAB: Self = Self(0x03);
    pub const RELA: Self = Self(0x04);
    pub const HASH: Self = Self(0x05);
    pub const DYNAMIC: Self = Self(0x06);
    pub const NOTE: Self = Self(0x07);
    pub const NOBITS: Self = Self(0x08);
    pub const REL: Self = Self(0x09);
    pub const SHLIB: Self = Self(0x0A);
    pub const DYNSYM: Self = Self(0x0B);
    pub const INITARRAY: Self = Self(0x0E);
    pub const FINIARRAY: Self = Self(0x0F);
    pub const PREINITARRAY: Self = Self(0x10);
    pub const GROUP: Self = Self(0x11);
    pub const SYMTABSHNDX: Self = Self(0x12);
    pub const NUM: Self = Self(0x13);
}

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct SectionHeaderFlags(pub u32);

impl SectionHeaderFlags {
    pub const WRITE: Self = Self(0x0001);
    pub const ALLOC: Self = Self(0x0002);
    pub const EXECINSTR: Self = Self(0x0004);
    pub const MERGE: Self = Self(0x0010);
    pub const STRINGS: Self = Self(0x0020);
    pub const INFOLINK: Self = Self(0x0040);
    pub const LINKORDER: Self = Self(0x0080);
    pub const OSNONCONFORMING: Self = Self(0x0100);
    pub const GROUP: Self = Self(0x0200);
    pub const TLS: Self = Self(0x0400);

    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        self.0 & other.0 > 0
    }
}

pub struct SymtabSection<'a> {
    header: &'a SectionHeader,
    data: &'a [u8],
}

impl SymtabSection<'_> {
    pub fn symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.data
            .chunks_exact(size_of::<Symbol>())
            .map(Symbol::try_ref_from_bytes)
            .map(Result::unwrap)
    }
}

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct Symbol {
    pub name: u32,
    pub value: usize,
    pub size: u32,
    pub info: u8,
    pub other: u8,
    pub shndx: u16,
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    #[cfg(not(miri))]
    use zerocopy::TryFromBytes;

    #[cfg(not(miri))]
    use crate::file::{
        ElfFile, ElfHeader, ElfIdent, ElfParseError, ElfType, ProgramHeaderType, SectionHeaderType,
    };

    // Helper to create minimal valid ELF header for testing
    fn create_minimal_valid_elf() -> [u8; 64] {
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(&[0x7f, 0x45, 0x4c, 0x46]); // ELF magic
        data[4] = 2; // 64-bit
        data[5] = 1; // little-endian
        data[6] = 1; // ELF version
        data[7] = 0; // OS ABI (System V)
        data[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        data[20..24].copy_from_slice(&1u32.to_le_bytes()); // version
        // shoff = 0 (no section headers)
        data[40..48].copy_from_slice(&0usize.to_le_bytes());
        data[52..54].copy_from_slice(&64u16.to_le_bytes()); // ehsize
        data[54..56].copy_from_slice(&56u16.to_le_bytes()); // phentsize
        data[56..58].copy_from_slice(&0u16.to_le_bytes()); // phnum = 0
        data[58..60].copy_from_slice(&64u16.to_le_bytes()); // shentsize
        data[60..62].copy_from_slice(&0u16.to_le_bytes()); // shnum = 0
        data[62..64].copy_from_slice(&0u16.to_le_bytes()); // shstrndx = 0
        data
    }

    #[cfg(not(miri))]
    #[test]
    fn test_elf_header_ref_from_bytes() {
        let data: [u8; 64] = [
            0x7f, 0x45, 0x4c, 0x46, // ELF magic
            0x02, // 64-bit
            0x01, // little-endian
            0x01, // ELF version
            0x06, // OS ABI
            0x07, // ABI Version
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // padding
            0x02, 0x00, // ET_EXEC (little endian)
            0x00, 0x00, // no specific instruction set
            0x01, 0x00, 0x00, 0x00, // ELF version 1
            0xE8, 0xE7, 0xE6, 0xE5, 0xE4, 0xE3, 0xE2, 0xE1, // entry point
            0xB8, 0xB7, 0xB6, 0xB5, 0xB4, 0xB3, 0xB2, 0xB1, // program header table offset
            0xC8, 0xC7, 0xC6, 0xC5, 0xC4, 0xC3, 0xC2, 0xC1, // section header table offset
            0xF4, 0xF3, 0xF2, 0xF1, // flags
            0x40, 0x00, // header size
            0x40, 0x00, // program header entry size
            0x22, 0x11, // num program headers
            0x40, 0x00, // section header entry size
            0x44, 0x33, // num section headers
            0x05, 0x00, // section names section header index
        ];

        let header = ElfHeader::try_ref_from_bytes(&data).unwrap();
        assert_eq!(
            header,
            &ElfHeader {
                ident: ElfIdent {
                    magic: [0x7f, 0x45, 0x4c, 0x46],
                    class: 2,
                    data: 1,
                    version: 1,
                    os_abi: 6,
                    abi_version: 7,
                    _padding: [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
                },
                typ: ElfType::Exec,
                machine: 0,
                version: 1,
                entry: 0xE1E2E3E4E5E6E7E8,
                phoff: 0xB1B2B3B4B5B6B7B8,
                shoff: 0xC1C2C3C4C5C6C7C8,
                flags: 0xF1F2F3F4,
                ehsize: 64,
                phentsize: 64,
                phnum: 0x1122,
                shentsize: 64,
                shnum: 0x3344,
                shstrndx: 5,
            }
        );
    }

    #[test]
    fn test_elf_file_parse_valid() {
        let data = create_minimal_valid_elf();
        let result = ElfFile::try_parse(&data);
        if let Err(e) = &result {
            panic!("Failed to parse ELF: {:?}. Data: {:?}", e, &data[50..64]);
        }
        let elf = result.unwrap();
        assert_eq!(elf.header.typ, ElfType::Exec);
        assert_eq!(elf.entry(), 0);
    }

    #[test]
    fn test_elf_file_parse_invalid_magic() {
        let mut data = create_minimal_valid_elf();
        data[0] = 0x00; // Corrupt magic
        let result = ElfFile::try_parse(&data);
        assert!(matches!(result, Err(ElfParseError::InvalidMagic)));
    }

    #[test]
    fn test_elf_file_parse_unsupported_endian() {
        let mut data = create_minimal_valid_elf();
        data[5] = 2; // big-endian on little-endian system
        let result = ElfFile::try_parse(&data);
        assert!(matches!(result, Err(ElfParseError::UnsupportedEndian)));
    }

    #[test]
    fn test_elf_file_parse_unsupported_version() {
        let mut data = create_minimal_valid_elf();
        data[6] = 2; // Invalid ELF version
        let result = ElfFile::try_parse(&data);
        assert!(matches!(result, Err(ElfParseError::UnsupportedElfVersion)));
    }

    #[test]
    fn test_elf_file_parse_unsupported_os_abi() {
        let mut data = create_minimal_valid_elf();
        data[7] = 0x03; // Not System V
        let result = ElfFile::try_parse(&data);
        assert!(matches!(result, Err(ElfParseError::UnsupportedOsAbi)));
    }

    #[test]
    fn test_elf_file_parse_invalid_phentsize() {
        // Start with a valid ELF and only change phentsize
        let data: [u8; 64] = [
            0x7f, 0x45, 0x4c, 0x46, // ELF magic
            0x02, // 64-bit
            0x01, // little-endian
            0x01, // ELF version
            0x00, // OS ABI (System V)
            0x00, // ABI Version
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // padding
            0x02, 0x00, // ET_EXEC
            0x00, 0x00, // machine
            0x01, 0x00, 0x00, 0x00, // ELF version 1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // entry point
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // program header offset
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // section header offset
            0x00, 0x00, 0x00, 0x00, // flags
            0x40, 0x00, // ehsize = 64
            0x20, 0x00, // phentsize = 32 (INVALID, should be 56)
            0x00, 0x00, // phnum = 0
            0x40, 0x00, // shentsize = 64
            0x00, 0x00, // shnum = 0
            0x00, 0x00, // shstrndx = 0
        ];

        let result = ElfFile::try_parse(&data);
        assert!(matches!(result, Err(ElfParseError::InvalidPhEntSize)));
    }

    #[test]
    fn test_elf_file_parse_invalid_shentsize() {
        // Start with a valid ELF and only change shentsize
        let data: [u8; 64] = [
            0x7f, 0x45, 0x4c, 0x46, // ELF magic
            0x02, // 64-bit
            0x01, // little-endian
            0x01, // ELF version
            0x00, // OS ABI (System V)
            0x00, // ABI Version
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // padding
            0x02, 0x00, // ET_EXEC
            0x00, 0x00, // machine
            0x01, 0x00, 0x00, 0x00, // ELF version 1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // entry point
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // program header offset
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // section header offset
            0x00, 0x00, 0x00, 0x00, // flags
            0x40, 0x00, // ehsize = 64
            0x38, 0x00, // phentsize = 56
            0x00, 0x00, // phnum = 0
            0x20, 0x00, // shentsize = 32 (INVALID, should be 64)
            0x00, 0x00, // shnum = 0
            0x00, 0x00, // shstrndx = 0
        ];

        let result = ElfFile::try_parse(&data);
        assert!(matches!(result, Err(ElfParseError::InvalidShEntSize)));
    }

    #[test]
    fn test_elf_file_entry() {
        let mut data = create_minimal_valid_elf();
        let entry_addr = 0x1000usize;
        data[24..32].copy_from_slice(&entry_addr.to_le_bytes());
        let elf = ElfFile::try_parse(&data).unwrap();
        assert_eq!(elf.entry(), entry_addr);
    }

    #[test]
    fn test_elf_file_program_headers() {
        let mut data = vec![0u8; 64 + 56 * 2]; // header + 2 program headers
        let header = create_minimal_valid_elf();
        data[..64].copy_from_slice(&header);

        // Set phoff and phnum
        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&2u16.to_le_bytes());

        // First program header: PT_LOAD
        let ph1_offset = 64;
        data[ph1_offset..ph1_offset + 4].copy_from_slice(&1u32.to_le_bytes());

        // Second program header: PT_DYNAMIC
        let ph2_offset = 64 + 56;
        data[ph2_offset..ph2_offset + 4].copy_from_slice(&2u32.to_le_bytes());

        let elf = ElfFile::try_parse(&data).unwrap();
        let headers: Vec<_> = elf.program_headers().collect();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].typ, ProgramHeaderType::LOAD);
        assert_eq!(headers[1].typ, ProgramHeaderType::DYNAMIC);
    }

    #[test]
    fn test_elf_file_program_headers_by_type() {
        let mut data = vec![0u8; 64 + 56 * 3];
        let header = create_minimal_valid_elf();
        data[..64].copy_from_slice(&header);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&3u16.to_le_bytes());

        // PT_LOAD
        data[64..68].copy_from_slice(&1u32.to_le_bytes());
        // PT_TLS
        data[120..124].copy_from_slice(&7u32.to_le_bytes());
        // PT_LOAD
        data[176..180].copy_from_slice(&1u32.to_le_bytes());

        let elf = ElfFile::try_parse(&data).unwrap();
        let load_headers: Vec<_> = elf
            .program_headers_by_type(ProgramHeaderType::LOAD)
            .collect();
        assert_eq!(load_headers.len(), 2);

        let tls_headers: Vec<_> = elf
            .program_headers_by_type(ProgramHeaderType::TLS)
            .collect();
        assert_eq!(tls_headers.len(), 1);
    }

    #[test]
    fn test_elf_file_section_headers() {
        let mut data = vec![0u8; 64 + 64 * 2]; // header + 2 section headers
        let header = create_minimal_valid_elf();
        data[..64].copy_from_slice(&header);

        // Set shoff and shnum
        data[40..48].copy_from_slice(&64usize.to_le_bytes());
        data[60..62].copy_from_slice(&2u16.to_le_bytes()); // shnum is at 60-62

        // First section header: NULL
        let sh1_offset = 64;
        data[sh1_offset + 4..sh1_offset + 8].copy_from_slice(&0u32.to_le_bytes());

        // Second section header: PROGBITS
        let sh2_offset = 64 + 64;
        data[sh2_offset + 4..sh2_offset + 8].copy_from_slice(&1u32.to_le_bytes());

        let elf = ElfFile::try_parse(&data).unwrap();
        let headers: Vec<_> = elf.section_headers().collect();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].typ, SectionHeaderType::NULL);
        assert_eq!(headers[1].typ, SectionHeaderType::PROGBITS);
    }

    #[test]
    fn test_elf_file_section_headers_by_type() {
        let mut data = vec![0u8; 64 + 64 * 3];
        let header = create_minimal_valid_elf();
        data[..64].copy_from_slice(&header);

        data[40..48].copy_from_slice(&64usize.to_le_bytes());
        data[60..62].copy_from_slice(&3u16.to_le_bytes()); // shnum is at 60-62

        // SYMTAB
        data[64 + 4..64 + 8].copy_from_slice(&2u32.to_le_bytes());
        // STRTAB
        data[128 + 4..128 + 8].copy_from_slice(&3u32.to_le_bytes());
        // SYMTAB
        data[192 + 4..192 + 8].copy_from_slice(&2u32.to_le_bytes());

        let elf = ElfFile::try_parse(&data).unwrap();
        let symtab_headers: Vec<_> = elf
            .section_headers_by_type(SectionHeaderType::SYMTAB)
            .collect();
        assert_eq!(symtab_headers.len(), 2);

        let strtab_headers: Vec<_> = elf
            .section_headers_by_type(SectionHeaderType::STRTAB)
            .collect();
        assert_eq!(strtab_headers.len(), 1);
    }

    #[test]
    fn test_elf_file_program_data() {
        let segment_data = b"Test Data";
        let mut data = vec![0u8; 64 + 56 + segment_data.len()];
        let header = create_minimal_valid_elf();
        data[..64].copy_from_slice(&header);

        data[32..40].copy_from_slice(&64usize.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Program header pointing to data
        let segment_offset = 64 + 56;
        data[segment_offset..segment_offset + segment_data.len()].copy_from_slice(segment_data);

        let ph_offset = 64;
        data[ph_offset + 8..ph_offset + 16].copy_from_slice(&segment_offset.to_le_bytes()); // offset
        data[ph_offset + 32..ph_offset + 40].copy_from_slice(&segment_data.len().to_le_bytes()); // filesz

        let elf = ElfFile::try_parse(&data).unwrap();
        let headers: Vec<_> = elf.program_headers().collect();
        let prog_data = elf.program_data(&headers[0]);
        assert_eq!(prog_data, segment_data);
    }

    #[test]
    fn test_elf_file_section_data() {
        let section_data = b"Section Content";
        let mut data = vec![0u8; 64 + 64 + section_data.len()];
        let header = create_minimal_valid_elf();
        data[..64].copy_from_slice(&header);

        data[40..48].copy_from_slice(&64usize.to_le_bytes());
        data[60..62].copy_from_slice(&1u16.to_le_bytes()); // shnum is at 60-62

        // Section data
        let section_offset = 64 + 64;
        data[section_offset..section_offset + section_data.len()].copy_from_slice(section_data);

        // Section header (offset field is at +24, size field is at +32)
        let sh_offset = 64;
        data[sh_offset + 24..sh_offset + 32].copy_from_slice(&section_offset.to_le_bytes()); // offset field
        data[sh_offset + 32..sh_offset + 40].copy_from_slice(&section_data.len().to_le_bytes()); // size field

        let elf = ElfFile::try_parse(&data).unwrap();
        let headers: Vec<_> = elf.section_headers().collect();
        let sec_data = elf.section_data(&headers[0]);
        assert_eq!(sec_data, section_data);
    }

    #[test]
    fn test_program_header_flags_contains() {
        use crate::file::ProgramHeaderFlags;

        let rwx = ProgramHeaderFlags(0x07);
        assert!(rwx.contains(&ProgramHeaderFlags::READABLE));
        assert!(rwx.contains(&ProgramHeaderFlags::WRITABLE));
        assert!(rwx.contains(&ProgramHeaderFlags::EXECUTABLE));

        let rx = ProgramHeaderFlags(0x05);
        assert!(rx.contains(&ProgramHeaderFlags::READABLE));
        assert!(!rx.contains(&ProgramHeaderFlags::WRITABLE));
        assert!(rx.contains(&ProgramHeaderFlags::EXECUTABLE));

        let none = ProgramHeaderFlags(0x00);
        assert!(!none.contains(&ProgramHeaderFlags::READABLE));
    }

    #[test]
    fn test_section_header_flags_contains() {
        use crate::file::SectionHeaderFlags;

        let flags = SectionHeaderFlags(SectionHeaderFlags::WRITE.0 | SectionHeaderFlags::ALLOC.0);
        assert!(flags.contains(&SectionHeaderFlags::WRITE));
        assert!(flags.contains(&SectionHeaderFlags::ALLOC));
        assert!(!flags.contains(&SectionHeaderFlags::EXECINSTR));
    }

    #[test]
    fn test_elf_type_variants() {
        assert_eq!(ElfType::None as u16, 0x00);
        assert_eq!(ElfType::Rel as u16, 0x01);
        assert_eq!(ElfType::Exec as u16, 0x02);
        assert_eq!(ElfType::Dyn as u16, 0x03);
        assert_eq!(ElfType::Core as u16, 0x04);
    }

    #[test]
    fn test_program_header_type_constants() {
        assert_eq!(ProgramHeaderType::NULL.0, 0x00);
        assert_eq!(ProgramHeaderType::LOAD.0, 0x01);
        assert_eq!(ProgramHeaderType::DYNAMIC.0, 0x02);
        assert_eq!(ProgramHeaderType::INTERP.0, 0x03);
        assert_eq!(ProgramHeaderType::NOTE.0, 0x04);
        assert_eq!(ProgramHeaderType::SHLIB.0, 0x05);
        assert_eq!(ProgramHeaderType::PHDR.0, 0x06);
        assert_eq!(ProgramHeaderType::TLS.0, 0x07);
    }

    #[test]
    fn test_section_header_type_constants() {
        assert_eq!(SectionHeaderType::NULL.0, 0x00);
        assert_eq!(SectionHeaderType::PROGBITS.0, 0x01);
        assert_eq!(SectionHeaderType::SYMTAB.0, 0x02);
        assert_eq!(SectionHeaderType::STRTAB.0, 0x03);
        assert_eq!(SectionHeaderType::NOBITS.0, 0x08);
    }
}
