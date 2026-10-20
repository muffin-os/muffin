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

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        let _ = ::core::fmt::Write::write_fmt(&mut $crate::Stdout, ::core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        let _ = ::core::fmt::Write::write_fmt(&mut $crate::Stdout, ::core::format_args!($($arg)*));
        let _ = ::core::fmt::Write::write_str(&mut $crate::Stdout, "\n");
    }};
}

#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {{
        let _ = ::core::fmt::Write::write_fmt(&mut $crate::Stderr, ::core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! eprintln {
    () => { $crate::eprint!("\n") };
    ($($arg:tt)*) => {{
        let _ = ::core::fmt::Write::write_fmt(&mut $crate::Stderr, ::core::format_args!($($arg)*));
        let _ = ::core::fmt::Write::write_str(&mut $crate::Stderr, "\n");
    }};
}
