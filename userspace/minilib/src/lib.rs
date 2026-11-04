#![no_std]

extern crate alloc;

mod backtrace;
mod io;
mod panic;
mod start;

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;

pub use io::{_print, Stderr, Stdout};
pub use muffin_sys::*;
pub use panic::catch_unwind;
pub use start::{__muffin_start_inner, args, env};

#[global_allocator]
static ALLOCATOR: GrowingHeap = GrowingHeap::new();

/// Returns the names in the directory at `path`, including `.` and `..`, in
/// the order the file system stores them.
///
/// # Errors
/// Returns the errno of `open` or `getdents`, such as `ENOENT` or `ENOTDIR`.
pub fn read_dir(path: &str) -> Result<Vec<String>, Errno> {
    let fd = open(path, O_RDONLY)?;
    let mut names = Vec::new();
    let result = read_dir_fd(fd, &mut names);
    let closed = close(fd);
    result.and(closed).map(|()| names)
}

fn read_dir_fd(fd: c_int, names: &mut Vec<String>) -> Result<(), Errno> {
    let mut buf = [0_u8; 1024];
    loop {
        let len = getdents(fd, &mut buf)?;
        if len == 0 {
            return Ok(());
        }
        let mut records = &buf[..len];
        while !records.is_empty() {
            let header = DirentHeader {
                name_len: records[0],
            };
            let body = &records[size_of::<DirentHeader>()..];
            let (name, rest) = body.split_at(header.name_len.into());
            // The kernel only lists names that are valid UTF-8.
            names.push(String::from_utf8_lossy(name).into_owned());
            records = rest;
        }
    }
}

