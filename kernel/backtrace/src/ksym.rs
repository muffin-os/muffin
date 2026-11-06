//! The `.ksym` kernel symbol file: a sorted table of function address ranges
//! and their demangled names.
//!
//! Layout, all integers little-endian: one [`Header`], then
//! `symbol_count` [`Entry`] values sorted by strictly ascending `addr`, then
//! `names_len` bytes of concatenated UTF-8 names without terminators.

use thiserror::Error;
use zerocopy::byteorder::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const MAGIC: [u8; 8] = *b"MUFFKSYM";
pub const VERSION: u32 = 1;

const _: () = {
    assert!(24 == size_of::<Header>());
    assert!(24 == size_of::<Entry>());
};

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Copy, Clone)]
#[repr(C)]
pub struct Header {
    pub magic: [u8; 8],
    pub version: U32,
    pub symbol_count: U32,
    pub names_len: U64,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Copy, Clone)]
#[repr(C)]
pub struct Entry {
    pub addr: U64,
    pub size: U64,
    /// Byte offset of the name in the name table.
    pub name_offset: U32,
    pub name_len: U32,
}

/// A validated view of a `.ksym` file.
pub struct KernelSymbols<'a> {
    entries: &'a [Entry],
    names: &'a [u8],
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ParseError {
    #[error("file is shorter than its header or symbol table")]
    Truncated,
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported ksym version {0}")]
    UnsupportedVersion(u32),
    #[error("name table length does not match the header")]
    NamesLengthMismatch,
    #[error("symbol {0} has an empty size")]
    EmptySymbol(u32),
    #[error("symbol {0} is not above its predecessor")]
    Unsorted(u32),
    #[error("symbol {0} has a name outside the name table or with invalid UTF-8")]
    BadName(u32),
}

/// The function that contains an address, and the distance of the address
/// from the start of that function.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Symbol<'a> {
    pub name: &'a str,
    pub offset: u64,
}

impl<'a> KernelSymbols<'a> {
    /// Validates `data` as a `.ksym` file. `data` may have any alignment.
    ///
    /// # Errors
    /// Returns the first violation of the file format that it finds.
    pub fn parse(data: &'a [u8]) -> Result<Self, ParseError> {
        let (header, rest) = Header::ref_from_prefix(data).map_err(|_| ParseError::Truncated)?;
        if header.magic != MAGIC {
            return Err(ParseError::BadMagic);
        }
        let version = header.version.get();
        if version != VERSION {
            return Err(ParseError::UnsupportedVersion(version));
        }

        let symbol_count = header.symbol_count.get() as usize;
        let (entries, names) = <[Entry]>::ref_from_prefix_with_elems(rest, symbol_count)
            .map_err(|_| ParseError::Truncated)?;
        if names.len() as u64 != header.names_len.get() {
            return Err(ParseError::NamesLengthMismatch);
        }

        let mut previous_addr = None;
        for (index, entry) in (0u32..).zip(entries) {
            if entry.size.get() == 0 {
                return Err(ParseError::EmptySymbol(index));
            }
            let addr = entry.addr.get();
            if previous_addr.is_some_and(|previous| addr <= previous) {
                return Err(ParseError::Unsorted(index));
            }
            previous_addr = Some(addr);

            let begin = entry.name_offset.get() as usize;
            let end = begin
                .checked_add(entry.name_len.get() as usize)
                .ok_or(ParseError::BadName(index))?;
            let name = names.get(begin..end).ok_or(ParseError::BadName(index))?;
            core::str::from_utf8(name).map_err(|_| ParseError::BadName(index))?;
        }

        Ok(Self { entries, names })
    }

    #[must_use]
    pub fn symbol_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns the symbol whose range contains `address`, or `None` if no
    /// symbol covers it.
    #[must_use]
    pub fn resolve(&self, address: u64) -> Option<Symbol<'a>> {
        let index = self.entries.partition_point(|e| e.addr.get() <= address);
        let entry = self.entries.get(index.checked_sub(1)?)?;
        let offset = address - entry.addr.get();
        if offset >= entry.size.get() {
            return None;
        }

        let begin = entry.name_offset.get() as usize;
        let name = self
            .names
            .get(begin..begin + entry.name_len.get() as usize)?;
        // SAFETY: parse validated every name range as UTF-8
        let name = unsafe { core::str::from_utf8_unchecked(name) };
        Some(Symbol { name, offset })
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use zerocopy::byteorder::little_endian::{U32, U64};
    use zerocopy::{FromBytes, IntoBytes};

    use super::{Entry, Header, KernelSymbols, MAGIC, ParseError, Symbol, VERSION};
    use crate::{Backtrace, Frame};

    const HEADER_SIZE: usize = size_of::<Header>();

    fn ksym(symbols: &[(u64, u64, &str)]) -> Vec<u8> {
        let mut entries = Vec::new();
        let mut names = Vec::new();
        for &(addr, size, name) in symbols {
            entries.push(Entry {
                addr: U64::new(addr),
                size: U64::new(size),
                name_offset: U32::new(names.len() as u32),
                name_len: U32::new(name.len() as u32),
            });
            names.extend_from_slice(name.as_bytes());
        }
        let header = Header {
            magic: MAGIC,
            version: U32::new(VERSION),
            symbol_count: U32::new(entries.len() as u32),
            names_len: U64::new(names.len() as u64),
        };

        let mut data = Vec::new();
        data.extend_from_slice(header.as_bytes());
        data.extend_from_slice(entries.as_bytes());
        data.extend_from_slice(&names);
        data
    }

    fn parse_error(data: &[u8]) -> ParseError {
        KernelSymbols::parse(data).err().unwrap()
    }

    fn rewrite_header(data: &mut [u8], edit: impl FnOnce(&mut Header)) {
        let mut header = Header::read_from_prefix(data).unwrap().0;
        edit(&mut header);
        data[..HEADER_SIZE].copy_from_slice(header.as_bytes());
    }

    fn two_symbols() -> Vec<u8> {
        ksym(&[(0x1000, 0x10, "a"), (0x1020, 0x8, "b")])
    }

    #[test]
    fn empty_input_is_truncated() {
        assert_eq!(parse_error(&[]), ParseError::Truncated);
    }

    #[test]
    fn wrong_magic_is_rejected() {
        let mut data = ksym(&[]);
        data[0] = b'X';
        assert_eq!(parse_error(&data), ParseError::BadMagic);
    }

    #[test]
    fn other_version_is_rejected() {
        let mut data = ksym(&[]);
        rewrite_header(&mut data, |h| h.version = U32::new(2));
        assert_eq!(parse_error(&data), ParseError::UnsupportedVersion(2));
    }

    #[test]
    fn symbol_count_past_the_end_is_truncated() {
        let mut data = ksym(&[(0x1000, 0x10, "a")]);
        rewrite_header(&mut data, |h| h.symbol_count = U32::new(3));
        assert_eq!(parse_error(&data), ParseError::Truncated);
    }

    #[test]
    fn names_length_off_by_one_is_rejected() {
        let mut data = ksym(&[(0x1000, 0x10, "a")]);
        rewrite_header(&mut data, |h| h.names_len = U64::new(2));
        assert_eq!(parse_error(&data), ParseError::NamesLengthMismatch);
    }

    #[test]
    fn descending_addresses_are_rejected() {
        let data = ksym(&[(0x20, 0x4, "a"), (0x10, 0x4, "b")]);
        assert_eq!(parse_error(&data), ParseError::Unsorted(1));
    }

    #[test]
    fn duplicate_addresses_are_rejected() {
        let data = ksym(&[(0x10, 0x4, "a"), (0x10, 0x4, "b")]);
        assert_eq!(parse_error(&data), ParseError::Unsorted(1));
    }

    #[test]
    fn zero_size_symbol_is_rejected() {
        let data = ksym(&[(0x10, 0, "a")]);
        assert_eq!(parse_error(&data), ParseError::EmptySymbol(0));
    }

    #[test]
    fn name_past_the_table_is_rejected() {
        let mut data = ksym(&[(0x10, 0x4, "a")]);
        let entry = Entry::mut_from_prefix(&mut data[HEADER_SIZE..]).unwrap().0;
        entry.name_len = U32::new(2);
        assert_eq!(parse_error(&data), ParseError::BadName(0));
    }

    #[test]
    fn name_with_invalid_utf8_is_rejected() {
        let mut data = ksym(&[(0x10, 0x4, "a")]);
        *data.last_mut().unwrap() = 0xff;
        assert_eq!(parse_error(&data), ParseError::BadName(0));
    }

    #[test]
    fn misaligned_input_parses() {
        let data = two_symbols();
        let mut shifted = std::vec![0u8];
        shifted.extend_from_slice(&data);
        let symbols = KernelSymbols::parse(&shifted[1..]).unwrap();
        assert_eq!(symbols.resolve(0x1020).unwrap().name, "b");
    }

    #[test]
    fn resolve_finds_the_containing_symbol() {
        let data = two_symbols();
        let symbols = KernelSymbols::parse(&data).unwrap();
        let at = |name, offset| Some(Symbol { name, offset });

        assert_eq!(symbols.resolve(0x0fff), None);
        assert_eq!(symbols.resolve(0x1000), at("a", 0));
        assert_eq!(symbols.resolve(0x100f), at("a", 0xf));
        assert_eq!(symbols.resolve(0x1010), None);
        assert_eq!(symbols.resolve(0x1020), at("b", 0));
        assert_eq!(symbols.resolve(0x1027), at("b", 7));
        assert_eq!(symbols.resolve(0x1028), None);
        assert_eq!(symbols.resolve(u64::MAX), None);
    }

    #[test]
    fn empty_table_resolves_nothing() {
        let data = ksym(&[]);
        let symbols = KernelSymbols::parse(&data).unwrap();
        assert_eq!(symbols.symbol_count(), 0);
        assert_eq!(symbols.resolve(0x1000), None);
    }

    #[test]
    fn backtrace_resolves_the_byte_before_each_return_address() {
        let data = two_symbols();
        let symbols = KernelSymbols::parse(&data).unwrap();
        let names: Vec<_> =
            Backtrace::new([0x1001usize, 0x1010, 0x1021].into_iter(), Some(&symbols))
                .map(|frame| frame.symbol.map(|s| s.name))
                .collect();
        assert_eq!(names, [Some("a"), Some("a"), Some("b")]);
    }

    #[test]
    fn backtrace_without_symbols_keeps_addresses() {
        let frames: Vec<_> = Backtrace::new([0x1001usize, 0x1021].into_iter(), None).collect();
        assert_eq!(
            frames
                .iter()
                .map(|f| f.instruction_pointer)
                .collect::<Vec<_>>(),
            [0x1001, 0x1021]
        );
        assert!(frames.iter().all(|f| f.symbol.is_none()));
    }

    #[test]
    fn frame_display_shows_address_and_symbol() {
        let resolved = Frame {
            instruction_pointer: 0x1021,
            symbol: Some(Symbol {
                name: "b",
                offset: 0,
            }),
        };
        let unknown = Frame {
            instruction_pointer: 0x1021,
            symbol: None,
        };
        assert_eq!(std::format!("{resolved}"), "0x0000000000001021 @ b+0x0");
        assert_eq!(std::format!("{unknown}"), "0x0000000000001021 @ <unknown>");
    }
}
