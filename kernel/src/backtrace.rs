use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::arch::asm;
use core::fmt::{Debug, Display, Formatter};
use core::iter;

use addr2line::gimli::{EndianSlice, Error};
use addr2line::{Context, gimli};
use conquer_once::spin::OnceCell;
use elf::ParseError;
use thiserror::Error;

use crate::UsizeExt;

static BACKTRACE_CONTEXT: OnceCell<BacktraceContext> = OnceCell::uninit();

struct BacktraceContext(Context<EndianSlice<'static, addr2line::gimli::NativeEndian>>);

unsafe impl Sync for BacktraceContext {}
unsafe impl Send for BacktraceContext {}

/// Initializes the backtrace context that is required for backtraces
/// with debug information during panics.
///
/// # Panics
/// This function panics if the kernel elf file cannot be found or if something goes wrong
/// during parsing.
pub fn init() {
    // TODO: make this work in release builds as well
    #[cfg(all(debug_assertions, feature = "backtrace"))]
    BACKTRACE_CONTEXT.init_once(|| {
        use core::slice::from_raw_parts;

        use addr2line::gimli::Dwarf;
        use elf::ElfBytes;
        use tracing::debug;
        use x86_64::VirtAddr;

        use crate::U64Ext;
        use crate::limine::KERNEL_FILE_REQUEST;

        debug!("initializing backtrace context");
        let kernel_file = KERNEL_FILE_REQUEST.get_response().unwrap();
        let file_addr = VirtAddr::from_ptr(kernel_file.file().addr());
        let file_size = kernel_file.file().size().into_usize();
        let file_slice = unsafe {
            // Safety: we keep the part of limine's higher half mapping that contains
            // the kernel file, so dereferencing that pointer is safe.
            from_raw_parts(file_addr.as_mut_ptr::<u8>(), file_size)
        };
        let file = ElfBytes::<elf::endian::NativeEndian>::minimal_parse(file_slice).unwrap();
        let dwarf = Dwarf::load(|section| {
            Ok::<_, ParseError>(EndianSlice::new(
                {
                    match file.section_header_by_name(section.name())? {
                        Some(h) => file.section_data(&h)?.0,
                        None => &[],
                    }
                },
                addr2line::gimli::NativeEndian,
            ))
        })
        .unwrap();
        let ctx = Context::from_dwarf(dwarf).unwrap();
        BacktraceContext(ctx)
    });
}

#[derive(Debug, Error)]
pub enum CaptureBacktraceError {
    #[error("no symbol file available")]
    NoSymbolFile,
    #[error("failed to parse symbol file")]
    ElfParseError(#[from] ParseError),
    #[error("failed to parse dwarf data")]
    DwarfParseError(gimli::Error),
}

impl From<gimli::Error> for CaptureBacktraceError {
    fn from(value: Error) -> Self {
        Self::DwarfParseError(value)
    }
}

pub struct Backtrace {
    frames: Vec<Frame>,
}

impl Backtrace {
    /// # Errors
    /// This function returns an error if the symbol file cannot be found or if
    /// the symbol file cannot be parsed.
    ///
    /// # Panics
    /// This function panics if frames for an instruction pointer cannot be found.
    pub fn try_capture() -> Result<Self, CaptureBacktraceError> {
        let ctx = &BACKTRACE_CONTEXT
            .get()
            .ok_or(CaptureBacktraceError::NoSymbolFile)?
            .0;
        let frames = ReturnAddressIterator::new()
            .flat_map(|ip| {
                let mut it = ctx
                    .find_frames(ip.saturating_sub(1).into_u64())
                    .skip_all_loads()
                    .unwrap();
                iter::from_fn(move || it.next().unwrap().map(|frame| (ip, frame)))
            })
            .map(|(ip, frame)| Frame {
                instruction_pointer: ip,
                kind: if let Some(function) = frame.function {
                    Kind::Resolved {
                        function_name: function.demangle().unwrap().to_string(),
                        file_name: frame
                            .location
                            .as_ref()
                            .map_or("<unknown>".to_string(), |l| l.file.unwrap().to_string()),
                        line_number: frame.location.as_ref().and_then(|l| l.line),
                        column_number: frame.location.as_ref().and_then(|l| l.column),
                    }
                } else {
                    Kind::Unknown
                },
            })
            .collect::<Vec<_>>();

        Ok(Self { frames })
    }
}

impl Display for Backtrace {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        for (i, frame) in self.frames.iter().enumerate() {
            write!(f, "\t{i:>2}: ")?;
            match &frame.kind {
                Kind::Resolved {
                    function_name,
                    file_name,
                    line_number,
                    column_number,
                } => {
                    writeln!(
                        f,
                        "{:p} @ {function_name}",
                        frame.instruction_pointer as *const usize
                    )?;
                    let line = line_number.unwrap_or(0);
                    let column = column_number.unwrap_or(0);
                    writeln!(f, "\t\tat {file_name}:{line}:{column}")?;
                }
                Kind::Unknown => write!(
                    f,
                    "{:p} @ <unknown>",
                    frame.instruction_pointer as *const usize
                )?,
            }
        }
        Ok(())
    }
}

struct Frame {
    instruction_pointer: usize,
    kind: Kind,
}

enum Kind {
    Unknown,
    Resolved {
        function_name: String,
        file_name: String,
        line_number: Option<u32>,
        column_number: Option<u32>,
    },
}

struct ReturnAddressIterator {
    current_bp: *const usize,
}

impl ReturnAddressIterator {
    pub fn new() -> Self {
        let mut current_bp: *const usize;
        unsafe {
            asm!(
            "mov {bp}, rbp",
            bp = out(reg) current_bp,
            );
        }
        Self { current_bp }
    }
}

impl Iterator for ReturnAddressIterator {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_bp.is_null() {
            return None;
        }

        let current_bp = self.current_bp;
        let next_bp = unsafe { *current_bp };
        let instruction_pointer = unsafe { *(current_bp.add(1)) };

        self.current_bp = next_bp as *const usize;
        if instruction_pointer == 0 {
            None
        } else {
            Some(instruction_pointer)
        }
    }
}
