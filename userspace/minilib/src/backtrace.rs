//! Symbolized stack backtrace for the panic report.
//!
//! Neither `.symtab` nor the debug sections carry `SHF_ALLOC`, so the loader never
//! maps them and the process maps its own executable read only to reach them.
//!
//! DWARF gives a source position and covers inlined functions. An optimized build
//! ships none, leaving `.symtab` to name the enclosing function alone.

use core::ffi::{c_int, c_void};
use core::fmt::Write;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use core::sync::atomic::{AtomicPtr, AtomicUsize};
use core::{ptr, slice};

use addr2line::Context;
use addr2line::gimli::{Dwarf, EndianSlice, NativeEndian};
use elf::ElfBytes;
use elf::string_table::StringTable;
use elf::symbol::SymbolTable;
use unwinding::abi::{_Unwind_Backtrace, _Unwind_GetIPInfo, UnwindContext, UnwindReasonCode};

use crate::{MapFlags, ProtFlags, Stat};

/// Frames collected before the walk is cut short, bounding a runaway recursion.
const MAX_FRAMES: usize = 32;

const PATH_MAX: usize = 4096;

/// Indent that lines an inlined frame up under the address above it, so it has to
/// match the width of the `  NN: 0x{:016x}` prefix.
const ADDRESS_COLUMN: usize = 24;

/// Names for return addresses, from whichever of the two tables the executable
/// still carries.
struct Symbols<'a> {
    dwarf: Option<Context<EndianSlice<'a, NativeEndian>>>,
    symtab: Option<(SymbolTable<'a, elf::endian::NativeEndian>, StringTable<'a>)>,
}

impl<'a> Symbols<'a> {
    /// Mangled name of the function containing `ip`.
    ///
    /// A zero size covers no address, which keeps a section marker or an assembly
    /// label from claiming a frame.
    fn enclosing_function(&self, ip: u64) -> Option<&'a str> {
        let (symbols, strings) = self.symtab.as_ref()?;
        let symbol = symbols.iter().find(|symbol| {
            symbol.st_symtype() == elf::abi::STT_FUNC
                && symbol.st_value <= ip
                && ip < symbol.st_value + symbol.st_size
        })?;
        strings.get(symbol.st_name as usize).ok()
    }
}

struct Trace {
    ips: [usize; MAX_FRAMES],
    len: usize,
}

/// Writes a backtrace of the caller's stack to `out`.
///
/// Every stage degrades on its own. An executable that cannot be read or parsed
/// leaves bare addresses, which `addr2line -e <binary>` resolves on a host with no
/// rebasing, since userspace binaries are not relocatable.
pub(crate) fn print(out: &mut impl Write) {
    let mut trace = Trace {
        ips: [0; MAX_FRAMES],
        len: 0,
    };
    _Unwind_Backtrace(collect, (&raw mut trace).cast());

    let _ = writeln!(out, "stack backtrace:");
    let symbols = map_own_image().and_then(load_symbols);
    for (index, &ip) in trace.ips[..trace.len].iter().enumerate() {
        write_frame(out, index, ip, symbols.as_ref());
    }
}

extern "C" fn collect(ctx: &UnwindContext<'_>, arg: *mut c_void) -> UnwindReasonCode {
    // SAFETY: `arg` is the `&raw mut Trace` handed to `_Unwind_Backtrace`, which
    // outlives the walk and is reached from nowhere else while it runs.
    let trace = unsafe { &mut *arg.cast::<Trace>() };
    if trace.len == MAX_FRAMES {
        return UnwindReasonCode::NORMAL_STOP;
    }

    let mut before_insn: c_int = 0;
    let ip = _Unwind_GetIPInfo(ctx, &mut before_insn);
    // The kernel enters `_start` over a zeroed stack slot, so the frame above it
    // carries a null return address. Address zero resolves to whichever DWARF unit
    // starts there, which is a confidently wrong name.
    if ip == 0 {
        return UnwindReasonCode::NORMAL_STOP;
    }
    // A return address points past the call, which for a call in tail position lands
    // in the next function. One byte back names the caller and its line.
    trace.ips[trace.len] = if before_insn == 0 {
        ip.saturating_sub(1)
    } else {
        ip
    };
    trace.len += 1;
    UnwindReasonCode::NO_REASON
}

/// Maps the running executable read only, once per process.
///
/// The mapping is lazy, so it commits a frame only for the pages the ELF and
/// DWARF parse touch. This runs on the panic path, where committing a
/// multi-megabyte image in physical frames can itself fail.
///
/// The result is cached because there is no munmap syscall and `catch_unwind`
/// lets a process print several backtraces, each of which would otherwise
/// strand another mapping of the whole image.
fn map_own_image() -> Option<&'static [u8]> {
    static IMAGE: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());
    static LEN: AtomicUsize = AtomicUsize::new(0);

    let cached = IMAGE.load(Acquire);
    if !cached.is_null() {
        // SAFETY: the pointer was published after its length, and no munmap
        // syscall exists, so the mapping outlives every borrow of it.
        return Some(unsafe { slice::from_raw_parts(cached, LEN.load(Relaxed)) });
    }

    let mut path = [0u8; PATH_MAX];
    let written = crate::exe_path(&mut path).ok()?;
    let path = core::str::from_utf8(&path[..written]).ok()?;

    let fd = crate::open(path).ok()?;

    let mut stat = Stat::default();
    crate::fstat(fd, &mut stat).ok()?;
    if stat.size == 0 {
        return None;
    }
    let len = stat.size as usize;

    let ptr = crate::mmap(0, len, ProtFlags::READ, MapFlags::PRIVATE, fd as usize, 0).ok()?;

    LEN.store(len, Relaxed);
    IMAGE.store(ptr, Release);
    // SAFETY: the mapping covers `len` readable bytes and is never unmapped.
    Some(unsafe { slice::from_raw_parts(ptr, len) })
}

fn load_symbols(image: &[u8]) -> Option<Symbols<'_>> {
    let elf = ElfBytes::<elf::endian::NativeEndian>::minimal_parse(image).ok()?;
    let dwarf = Dwarf::load(|id| {
        let data = match elf.section_header_by_name(id.name())? {
            Some(header) => match elf.section_data(&header)? {
                (data, None) => data,
                // An empty section is what `Dwarf::load` expects for one it cannot
                // use, and inflating this would need a decompressor.
                (_, Some(_)) => &[],
            },
            None => &[],
        };
        Ok::<_, elf::ParseError>(EndianSlice::new(data, NativeEndian))
    });
    Some(Symbols {
        dwarf: dwarf.ok().and_then(|dwarf| Context::from_dwarf(dwarf).ok()),
        symtab: elf.symbol_table().ok().flatten(),
    })
}

/// Renders one address, plus a line per function inlined into it.
fn write_frame(out: &mut impl Write, index: usize, ip: usize, symbols: Option<&Symbols<'_>>) {
    let mut printed = 0;
    if let Some(dwarf) = symbols.and_then(|symbols| symbols.dwarf.as_ref())
        && let Ok(mut frames) = dwarf.find_frames(ip as u64).skip_all_loads()
    {
        while let Ok(Some(frame)) = frames.next() {
            let _ = if printed == 0 {
                write!(out, "  {index:>2}: {ip:#018x} ")
            } else {
                write!(out, "{:ADDRESS_COLUMN$} ", "")
            };
            printed += 1;

            match frame
                .function
                .as_ref()
                .and_then(|name| name.demangle().ok())
            {
                Some(name) => {
                    let _ = write!(out, "{name}");
                }
                None => {
                    let _ = write!(out, "<unknown>");
                }
            }
            if let Some(location) = frame.location
                && let Some(file) = location.file
            {
                let line = location.line.unwrap_or(0);
                let column = location.column.unwrap_or(0);
                let _ = write!(out, " at {file}:{line}:{column}");
            }
            let _ = writeln!(out);
        }
    }
    if printed > 0 {
        return;
    }

    let _ = write!(out, "  {index:>2}: {ip:#018x} ");
    match symbols.and_then(|symbols| symbols.enclosing_function(ip as u64)) {
        Some(name) => {
            // The alternate form drops the trailing hash the mangling carries.
            let _ = writeln!(out, "{:#}", rustc_demangle::demangle(name));
        }
        None => {
            let _ = writeln!(out, "<unknown>");
        }
    }
}
