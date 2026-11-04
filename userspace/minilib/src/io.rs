use core::ffi::c_int;
use core::fmt::Write;

/// Writer over fd 1.
pub struct Stdout;

/// Writer over fd 2.
pub struct Stderr;

// The write syscall rejects a zero-length buffer, and formatters emit empty
// pieces freely, so empty input writes nothing.
// A short write is retried on the rest so a FAIL marker is never clipped.
fn emit(fd: c_int, s: &str) -> core::fmt::Result {
    let mut rest = s.as_bytes();
    while !rest.is_empty() {
        match crate::write(fd, rest) {
            Ok(n) if n > 0 => rest = &rest[n..],
            _ => return Err(core::fmt::Error),
        }
    }
    Ok(())
}

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        emit(1, s)
    }
}

impl Write for Stderr {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        emit(2, s)
    }
}

/// The kernel holds the serial lock for one write syscall, so a line written
/// in one call cannot be split by kernel log lines or other processes.
#[doc(hidden)]
pub fn _print(fd: c_int, args: core::fmt::Arguments) {
    let _ = emit(fd, &alloc::fmt::format(args));
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::_print(1, ::core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {
        $crate::_print(1, ::core::format_args!("{}\n", ::core::format_args!($($arg)*)))
    };
}

#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {
        $crate::_print(2, ::core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! eprintln {
    () => { $crate::eprint!("\n") };
    ($($arg:tt)*) => {
        $crate::_print(2, ::core::format_args!("{}\n", ::core::format_args!($($arg)*)))
    };
}
