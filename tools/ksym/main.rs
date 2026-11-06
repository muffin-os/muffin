//! Extracts the function symbols of the kernel ELF into a `.ksym` file.
//!
//! Usage: `ksym <kernel-elf> <out.ksym>`

use std::collections::BTreeMap;
use std::collections::btree_map::Entry as MapEntry;
use std::error::Error;
use std::{env, fs};

use elf::ElfBytes;
use elf::abi::{STB_GLOBAL, STT_FUNC};
use elf::endian::AnyEndian;
use kernel_backtrace::{Entry, Header, MAGIC, VERSION};
use zerocopy::IntoBytes;
use zerocopy::byteorder::little_endian::{U32, U64};

struct Function {
    size: u64,
    name: String,
    is_global: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    let [_, input, output] = args.as_slice() else {
        return Err("usage: ksym <kernel-elf> <out.ksym>".into());
    };

    let image = fs::read(input)?;
    let elf = ElfBytes::<AnyEndian>::minimal_parse(&image)?;
    let (symtab, strtab) = elf.symbol_table()?.ok_or("kernel ELF has no .symtab")?;

    let mut functions = BTreeMap::<u64, Function>::new();
    for symbol in symtab
        .iter()
        .filter(|s| s.st_symtype() == STT_FUNC && s.st_size > 0 && !s.is_undefined())
    {
        let function = Function {
            size: symbol.st_size,
            // The alternate form drops the trailing hash the mangling carries.
            name: format!(
                "{:#}",
                rustc_demangle::demangle(strtab.get(symbol.st_name as usize)?)
            ),
            is_global: symbol.st_bind() == STB_GLOBAL,
        };
        // Identical code folding gives one address several names. Keep the
        // global one, else the first seen.
        match functions.entry(symbol.st_value) {
            MapEntry::Vacant(slot) => {
                slot.insert(function);
            }
            MapEntry::Occupied(mut slot) => {
                if function.is_global && !slot.get().is_global {
                    slot.insert(function);
                }
            }
        }
    }

    write_ksym(output, &functions)
}

fn write_ksym(output: &str, functions: &BTreeMap<u64, Function>) -> Result<(), Box<dyn Error>> {
    let mut entries = Vec::with_capacity(functions.len());
    let mut names = Vec::new();
    for (&addr, function) in functions {
        entries.push(Entry {
            addr: U64::new(addr),
            size: U64::new(function.size),
            name_offset: U32::new(u32::try_from(names.len())?),
            name_len: U32::new(u32::try_from(function.name.len())?),
        });
        names.extend_from_slice(function.name.as_bytes());
    }
    // Entry offsets are u32, so the name table must stay addressable by one.
    u32::try_from(names.len()).map_err(|_| "name table exceeds u32::MAX bytes")?;

    let header = Header {
        magic: MAGIC,
        version: U32::new(VERSION),
        symbol_count: U32::new(
            u32::try_from(entries.len()).map_err(|_| "more than u32::MAX symbols")?,
        ),
        names_len: U64::new(names.len() as u64),
    };

    let mut data = Vec::with_capacity(size_of::<Header>() + entries.as_bytes().len() + names.len());
    data.extend_from_slice(header.as_bytes());
    data.extend_from_slice(entries.as_bytes());
    data.extend_from_slice(&names);
    fs::write(output, data)?;
    Ok(())
}
