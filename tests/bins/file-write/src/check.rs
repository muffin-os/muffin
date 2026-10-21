use minilib::{Errno, println, read};

pub fn ok(name: &str) {
    println!("filewrite: {name} ok");
}

#[track_caller]
pub fn fail(name: &str) -> ! {
    panic!("filewrite: FAIL {name}")
}

#[track_caller]
pub fn require(name: &str, ok: bool) {
    if !ok {
        fail(name);
    }
}

#[track_caller]
pub fn expect_err<T>(name: &str, actual: Result<T, Errno>, expected: Errno) {
    match actual {
        Err(errno) if errno == expected => {}
        Err(errno) => panic!("filewrite: FAIL {name}, got {errno:?}, expected {expected:?}"),
        Ok(_) => panic!("filewrite: FAIL {name}, call succeeded, expected {expected:?}"),
    }
}

#[track_caller]
pub fn expect_ok<T: PartialEq>(name: &str, actual: Result<T, Errno>, expected: T) {
    match actual {
        Ok(value) if value == expected => {}
        Ok(_) => panic!("filewrite: FAIL {name}, wrong value"),
        Err(errno) => panic!("filewrite: FAIL {name}, got {errno:?}, expected success"),
    }
}

#[track_caller]
pub fn unwrap_or_fail<T>(name: &str, actual: Result<T, Errno>) -> T {
    match actual {
        Ok(value) => value,
        Err(errno) => panic!("filewrite: FAIL {name}, got {errno:?}"),
    }
}

#[track_caller]
pub fn read_to_end(name: &str, fd: i32, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buf.len() {
        match unwrap_or_fail(name, read(fd, &mut buf[filled..])) {
            0 => break,
            n => filled += n,
        }
    }
    filled
}
