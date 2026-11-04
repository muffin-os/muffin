//! This test program checks POSIX shared memory objects under `/dev/shm`.
//!
//! The steps run in order and share one object. The first failure prints
//! `shm: FAIL <step>` and exits 1 immediately. Success prints `shm: ok`.

use std::ffi::{c_char, c_int, c_uint};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::FromRawFd;
use std::process::exit;

use muffin_sys::{
    EBUSY, EINVAL, ENOENT, Errno, MapFlags, O_CREAT, O_EXCL, O_RDWR, ProtFlags, mmap, munmap,
};

unsafe extern "C" {
    fn shm_open(name: *const c_char, oflag: c_int, mode: c_uint) -> c_int;
    fn shm_unlink(name: *const c_char) -> c_int;
}

const PATH: &str = "/dev/shm/shm-test";
const LEN: usize = 8192;
const PATTERN_LEN: usize = 256;

fn errno() -> Option<c_int> {
    io::Error::last_os_error().raw_os_error()
}

fn is_errno(result: &io::Result<impl Sized>, expected: Errno) -> bool {
    result
        .as_ref()
        .err()
        .and_then(io::Error::raw_os_error)
        .is_some_and(|code| code == c_int::from(expected))
}

fn read_all(path: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn pattern_matches(bytes: &[u8]) -> bool {
    bytes.len() >= PATTERN_LEN
        && bytes[..PATTERN_LEN]
            .iter()
            .enumerate()
            .all(|(i, &b)| b == i as u8)
}

fn run() -> Result<(), &'static str> {
    let fd = unsafe { shm_open(c"/shm-test".as_ptr(), O_CREAT | O_EXCL | O_RDWR, 0o600) };
    if fd < 0 {
        return Err("shm_open");
    }

    let file = unsafe { File::from_raw_fd(fd) };
    if file.set_len(LEN as u64).is_err()
        || file.metadata().map(|m| m.len()).ok() != Some(LEN as u64)
    {
        return Err("set_len");
    }

    let Ok(addr) = mmap(
        0,
        LEN,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        fd,
        0,
    ) else {
        return Err("mmap");
    };
    for i in 0..PATTERN_LEN {
        unsafe { addr.add(i).write_volatile(i as u8) };
    }
    unsafe { addr.add(LEN - 1).write_volatile(0xAB) };

    let Ok(bytes) = read_all(PATH) else {
        return Err("read");
    };
    if bytes.len() != LEN || !pattern_matches(&bytes) || bytes[LEN - 1] != 0xAB {
        return Err("mapped writes visible through read");
    }

    if !is_errno(&file.set_len(4096), EBUSY) {
        return Err("set_len while mapped is EBUSY");
    }

    if unsafe { munmap(addr, LEN) }.is_err() {
        return Err("munmap");
    }
    if file.set_len(4096).is_err() || file.metadata().map(|m| m.len()).ok() != Some(4096) {
        return Err("set_len after munmap");
    }
    if !read_all(PATH).is_ok_and(|bytes| bytes.len() == 4096 && pattern_matches(&bytes)) {
        return Err("resize keeps the data");
    }

    if unsafe { munmap(addr, LEN) } != Err(EINVAL) {
        return Err("second munmap is EINVAL");
    }

    let mut survivor = File::open(PATH).map_err(|_| "open before unlink")?;
    if unsafe { shm_unlink(c"/shm-test".as_ptr()) } != 0 {
        return Err("shm_unlink");
    }
    if !is_errno(&File::open(PATH), ENOENT) {
        return Err("open after unlink is ENOENT");
    }
    let mut bytes = Vec::new();
    if survivor.read_to_end(&mut bytes).is_err() || bytes.len() != 4096 || !pattern_matches(&bytes)
    {
        return Err("open file survives unlink");
    }
    if unsafe { shm_unlink(c"/shm-test".as_ptr()) } != -1 || errno() != Some(c_int::from(ENOENT)) {
        return Err("second shm_unlink is ENOENT");
    }

    if !is_errno(&std::fs::remove_file(PATH), ENOENT) {
        return Err("remove_file after unlink is ENOENT");
    }
    if unsafe { shm_open(c"bad/name".as_ptr(), O_CREAT | O_RDWR, 0) } != -1
        || errno() != Some(c_int::from(EINVAL))
    {
        return Err("shm_open rejects a nested name");
    }
    Ok(())
}

fn main() {
    if let Err(step) = run() {
        println!("shm: FAIL {step}");
        exit(1);
    }
    println!("shm: ok");
}
