use minilib::{Errno, write};

pub fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

pub fn group(name: &str) {
    puts("posix: group ");
    puts(name);
    puts("\n");
}

#[track_caller]
pub fn fail(name: &str) -> ! {
    panic!("posix: FAIL {name}")
}

#[track_caller]
pub fn require(name: &str, ok: bool) {
    if !ok {
        fail(name);
    }
}

#[track_caller]
pub fn expect_errno(name: &str, actual: Errno, expected: Errno) {
    if actual != expected {
        panic!("posix: FAIL {name}, got {actual:?}, expected {expected:?}");
    }
}

#[track_caller]
pub fn expect_err<T>(name: &str, actual: Result<T, Errno>, expected: Errno) {
    match actual {
        Err(errno) => expect_errno(name, errno, expected),
        Ok(_) => panic!("posix: FAIL {name}, call succeeded, expected {expected:?}"),
    }
}

#[track_caller]
pub fn expect_ok<T: PartialEq>(name: &str, actual: Result<T, Errno>, expected: T) {
    match actual {
        Ok(value) if value == expected => {}
        Ok(_) => panic!("posix: FAIL {name}, wrong value"),
        Err(errno) => panic!("posix: FAIL {name}, got {errno:?}, expected success"),
    }
}

#[track_caller]
pub fn unwrap_or_fail<T>(name: &str, actual: Result<T, Errno>) -> T {
    match actual {
        Ok(value) => value,
        Err(errno) => panic!("posix: FAIL {name}, got {errno:?}"),
    }
}
