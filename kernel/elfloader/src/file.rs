use core::ffi::CStr;
use core::fmt::{Debug, Display, Formatter};
use core::ops::Range;

use thiserror::Error;
use zerocopy::{FromBytes, Immutable, KnownLayout, TryFromBytes};

#[derive(Copy, Clone, Debug)]
pub struct ElfFile<'a> {
    pub(crate) source: &'a [u8],
    pub(crate) header: ElfHeader,
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
    #[error("buffer shorter than the elf header")]
    TruncatedHeader,
    #[error("unsupported elf class")]
    UnsupportedClass,
    #[error("header table lies outside the buffer")]
    HeaderTableOutOfBounds,
}

impl<'a> ElfFile<'a> {
    /// # Errors
    /// Returns an error if the ELF file is invalid or not supported.
    pub fn try_parse(source: &'a [u8]) -> Result<Self, ElfParseError> {
        #[cfg(target_endian = "little")]
        const ENDIAN: u8 = 1;
        #[cfg(target_endian = "big")]
        const ENDIAN: u8 = 2;

        if source.len() < size_of::<ElfHeader>() {
            return Err(ElfParseError::TruncatedHeader);
        }

        let header = ElfHeader::try_read_from_bytes(&source[..size_of::<ElfHeader>()])
            .map_err(|_| ElfParseError::HeaderParseError)?;

        if header.ident.magic != [0x7F, 0x45, 0x4C, 0x46] {
            return Err(ElfParseError::InvalidMagic);
        }

        if header.ident.class != 2 {
            return Err(ElfParseError::UnsupportedClass);
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

    #[must_use]
    pub fn typ(&self) -> &ElfType {
        &self.header.typ
    }

    /// `None` if the table end does not fit in `usize`.
    #[must_use]
    pub fn program_header_table_end(&self) -> Option<usize> {
        table_range::<ProgramHeader>(self.header.phoff, self.header.phnum).map(|r| r.end)
    }

    /// # Errors
    /// Returns an error if the table is not contained in the source buffer.
    pub fn program_headers(
        &self,
    ) -> Result<impl Iterator<Item = ProgramHeader> + '_, ElfParseError> {
        self.headers(self.header.phoff, self.header.phnum)
    }

    /// # Errors
    /// Returns an error if the table is not contained in the source buffer.
    pub fn program_headers_by_type(
        &self,
        typ: ProgramHeaderType,
    ) -> Result<impl Iterator<Item = ProgramHeader> + '_, ElfParseError> {
        Ok(self.program_headers()?.filter(move |h| h.typ == typ))
    }

    /// # Errors
    /// Returns an error if the table is not contained in the source buffer.
    pub fn section_headers(
        &self,
    ) -> Result<impl Iterator<Item = SectionHeader> + '_, ElfParseError> {
        self.headers(self.header.shoff, self.header.shnum)
    }

    /// # Errors
    /// Returns an error if the table is not contained in the source buffer.
    pub fn section_headers_by_type(
        &self,
        typ: SectionHeaderType,
    ) -> Result<impl Iterator<Item = SectionHeader> + '_, ElfParseError> {
        Ok(self.section_headers()?.filter(move |h| h.typ == typ))
    }

    fn headers<T: FromBytes>(
        &self,
        offset: usize,
        num: u16,
    ) -> Result<impl Iterator<Item = T> + '_, ElfParseError> {
        let data = table_range::<T>(offset, num)
            .and_then(|r| self.source.get(r))
            .ok_or(ElfParseError::HeaderTableOutOfBounds)?;
        Ok(data.chunks_exact(size_of::<T>()).map(|c| {
            T::read_from_bytes(c).expect("chunks_exact yields exactly size_of::<T>() bytes")
        }))
    }

    /// `None` if the section's byte range is not contained in the source buffer.
    #[must_use]
    pub fn section_data(&self, header: &SectionHeader) -> Option<&[u8]> {
        self.source
            .get(header.offset..header.offset.checked_add(header.size)?)
    }

    #[must_use]
    pub fn section_name(&self, header: &SectionHeader) -> Option<&str> {
        let shstrtab = self
            .section_headers()
            .ok()?
            .nth(usize::from(self.header.shstrndx))?;
        let shstrtab_data = self.section_data(&shstrtab)?;
        CStr::from_bytes_until_nul(shstrtab_data.get(header.name as usize..)?)
            .ok()?
            .to_str()
            .ok()
    }

    /// # Errors
    /// Returns an error if the section header table is not contained in the source buffer.
    pub fn sections_by_name(
        &self,
        name: &'a str,
    ) -> Result<impl Iterator<Item = SectionHeader> + '_, ElfParseError> {
        Ok(self
            .section_headers()?
            .filter(move |h| self.section_name(h) == Some(name)))
    }

    /// `None` if the segment's file bytes are not contained in the source buffer.
    #[must_use]
    pub fn program_data(&self, header: &ProgramHeader) -> Option<&[u8]> {
        self.source
            .get(header.offset..header.offset.checked_add(header.filesz)?)
    }

    /// `None` if the section's byte range is not contained in the source buffer.
    #[must_use]
    pub fn symtab_data(&'a self, header: &'a SectionHeader) -> Option<SymtabSection<'a>> {
        let data = self.section_data(header)?;
        Some(SymtabSection { header, data })
    }

    #[must_use]
    pub fn symbol_name(&self, symtab: &SymtabSection<'a>, symbol: &Symbol) -> Option<&str> {
        let strtab_hdr = self
            .section_headers()
            .ok()?
            .nth(symtab.header.link as usize)?;
        let strtab_data = self.section_data(&strtab_hdr)?;
        CStr::from_bytes_until_nul(strtab_data.get(symbol.name as usize..)?)
            .ok()
            .and_then(|cstr| cstr.to_str().ok())
    }
}

fn table_range<T>(offset: usize, num: u16) -> Option<Range<usize>> {
    let len = usize::from(num).checked_mul(size_of::<T>())?;
    Some(offset..offset.checked_add(len)?)
}

const _: () = {
    assert!(64 == size_of::<ElfHeader>());
};

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq, Copy, Clone)]
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

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq, Copy, Clone)]
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

#[derive(TryFromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq, Copy, Clone)]
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

#[derive(FromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
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

#[derive(FromBytes, KnownLayout, Immutable, Eq, PartialEq, Copy, Clone)]
#[repr(transparent)]
pub struct ProgramHeaderType(pub u32);

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

#[derive(FromBytes, KnownLayout, Immutable, Eq, PartialEq, Copy, Clone)]
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

#[derive(FromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
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

#[derive(FromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq, Copy, Clone)]
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

#[derive(FromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
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
    pub fn symbols(&self) -> impl Iterator<Item = Symbol> + '_ {
        self.data
            .as_chunks::<{ size_of::<Symbol>() }>()
            .0
            .iter()
            .map(|c| {
                Symbol::read_from_bytes(c).expect("chunk is exactly size_of::<Symbol>() bytes")
            })
    }
}

#[derive(FromBytes, KnownLayout, Immutable, Debug, Eq, PartialEq)]
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
    #[cfg(not(miri))]
    use zerocopy::TryFromBytes;

    #[cfg(not(miri))]
    use crate::file::{ElfHeader, ElfIdent, ElfType};

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
    fn try_parse_rejects_short_buffer() {
        use crate::file::{ElfFile, ElfHeader, ElfParseError};

        let short = [0u8; size_of::<ElfHeader>() - 1];
        assert_eq!(
            ElfFile::try_parse(&short).unwrap_err(),
            ElfParseError::TruncatedHeader,
            "a buffer shorter than the elf header must be rejected before slicing"
        );
    }

    fn header(phoff: usize, phnum: u16, shoff: usize, shnum: u16) -> [u8; 64] {
        use crate::file::{ProgramHeader, SectionHeader};

        let mut h = [0u8; 64];
        h[..4].copy_from_slice(&[0x7f, 0x45, 0x4c, 0x46]);
        h[4] = 2; // ELFCLASS64
        h[5] = 1; // little-endian
        h[6] = 1;
        h[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        h[20..24].copy_from_slice(&1u32.to_le_bytes());
        h[32..40].copy_from_slice(&phoff.to_le_bytes());
        h[40..48].copy_from_slice(&shoff.to_le_bytes());
        h[52..54].copy_from_slice(&64u16.to_le_bytes());
        h[54..56].copy_from_slice(&(size_of::<ProgramHeader>() as u16).to_le_bytes());
        h[56..58].copy_from_slice(&phnum.to_le_bytes());
        h[58..60].copy_from_slice(&(size_of::<SectionHeader>() as u16).to_le_bytes());
        h[60..62].copy_from_slice(&shnum.to_le_bytes());
        h
    }

    fn load_phdr(p_type: u32) -> [u8; 56] {
        let mut p = [0u8; 56];
        p[..4].copy_from_slice(&p_type.to_le_bytes());
        p
    }

    #[test]
    fn try_parse_rejects_elf32_class() {
        use crate::file::{ElfFile, ElfParseError};

        let mut h = header(0, 0, 0, 0);
        h[4] = 1;
        assert_eq!(
            ElfFile::try_parse(&h).unwrap_err(),
            ElfParseError::UnsupportedClass
        );
    }

    #[test]
    fn program_header_table_end_rejects_overflow() {
        use crate::file::ElfFile;

        let h = header(usize::MAX - 10, 1, 0, 0);
        assert_eq!(
            ElfFile::try_parse(&h).unwrap().program_header_table_end(),
            None
        );
    }

    #[test]
    fn program_headers_reject_table_outside_buffer() {
        use crate::file::{ElfFile, ElfParseError};

        let h = header(64, 1, 0, 0);
        let elf = ElfFile::try_parse(&h).unwrap();
        assert_eq!(
            elf.program_headers().err(),
            Some(ElfParseError::HeaderTableOutOfBounds)
        );
        let h = header(0, 0, 64, 1);
        let elf = ElfFile::try_parse(&h).unwrap();
        assert_eq!(
            elf.section_headers().err(),
            Some(ElfParseError::HeaderTableOutOfBounds)
        );
    }

    #[test]
    fn section_data_outside_buffer_is_none() {
        use crate::file::{ElfFile, SectionHeader};

        let mut buf = [0u8; 128];
        buf[..64].copy_from_slice(&header(0, 0, 64, 1));
        buf[64 + 24..64 + 32].copy_from_slice(&200usize.to_le_bytes()); // sh_offset
        buf[64 + 32..64 + 40].copy_from_slice(&8usize.to_le_bytes()); // sh_size
        let elf = ElfFile::try_parse(&buf).unwrap();
        let sh: SectionHeader = elf.section_headers().unwrap().next().unwrap();
        assert_eq!(elf.section_data(&sh), None);
    }

    #[test]
    fn program_headers_parse_from_unaligned_buffer() {
        use crate::file::{ElfFile, ProgramHeaderType};

        #[repr(align(8))]
        struct Buf([u8; 1 + 64 + 56]);
        let mut buf = Buf([0; 1 + 64 + 56]);
        buf.0[1..65].copy_from_slice(&header(64, 1, 0, 0));
        buf.0[65..].copy_from_slice(&load_phdr(1));

        let elf = ElfFile::try_parse(&buf.0[1..]).expect("header parses from odd address");
        let mut headers = elf.program_headers().unwrap();
        assert_eq!(headers.next().map(|h| h.typ), Some(ProgramHeaderType::LOAD));
        assert!(headers.next().is_none());
    }

    #[test]
    fn program_header_type_is_32_bits() {
        use crate::file::{ElfFile, ProgramHeaderType};

        let mut buf = [0u8; 64 + 56];
        buf[..64].copy_from_slice(&header(64, 1, 0, 0));
        buf[64..].copy_from_slice(&load_phdr(0x0001_0001));
        let elf = ElfFile::try_parse(&buf).unwrap();
        assert_eq!(
            elf.program_headers_by_type(ProgramHeaderType::LOAD)
                .unwrap()
                .count(),
            0
        );
    }
}
