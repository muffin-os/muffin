#![no_std]
#![no_main]

mod check;

use minilib::{
    EBADF, EISDIR, O_APPEND, O_RDONLY, O_RDWR, O_WRONLY, Stat, Whence, fstat, lseek, open, write,
};

const SEED: &[u8] = b"muffin seed data\n";

const SUFFIX: &[u8] = b"appended\n";

minilib::entry!(main);

fn main() -> i32 {
    overwrite();
    append();
    dir();
    0
}

fn size_of(name: &str, fd: i32) -> u64 {
    let mut stat = Stat::default();
    check::expect_ok(name, fstat(fd, &mut stat), ());
    stat.size
}

fn overwrite() {
    let fd = check::unwrap_or_fail("overwrite/open", open("/data/over.txt", O_RDWR));
    check::expect_ok("overwrite/lseek", lseek(fd, 3, Whence::Set), 3);
    check::expect_ok("overwrite/write", write(fd, b"XY"), 2);

    let mut expected = [0_u8; SEED.len()];
    expected.copy_from_slice(SEED);
    expected[3..5].copy_from_slice(b"XY");
    let ro = check::unwrap_or_fail("overwrite/reopen", open("/data/over.txt", O_RDONLY));
    let mut buf = [0_u8; SEED.len() + 1];
    let n = check::read_to_end("overwrite/read", ro, &mut buf);
    check::require("overwrite/contents", buf[..n] == expected);
    check::ok("overwrite");
}

fn append() {
    let fd = check::unwrap_or_fail("append/open", open("/data/seed.txt", O_WRONLY | O_APPEND));
    // O_APPEND ignores the position, so this seek must not move the write.
    check::expect_ok("append/lseek_set", lseek(fd, 0, Whence::Set), 0);
    check::expect_ok("append/write", write(fd, SUFFIX), SUFFIX.len());
    let len = (SEED.len() + SUFFIX.len()) as u64;
    check::expect_ok("append/lseek_cur", lseek(fd, 0, Whence::Cur), len);
    check::require("append/fstat_size", size_of("append/fstat", fd) == len);

    let ro = check::unwrap_or_fail("append/reopen", open("/data/seed.txt", O_RDONLY));
    check::require(
        "append/fstat_reopen_size",
        size_of("append/fstat_reopen", ro) == len,
    );
    let mut buf = [0_u8; SEED.len() + SUFFIX.len() + 1];
    let n = check::read_to_end("append/read", ro, &mut buf);
    check::require(
        "append/contents",
        &buf[..SEED.len()] == SEED && &buf[SEED.len()..n] == SUFFIX,
    );
    check::ok("append");
}

fn dir() {
    let ro = check::unwrap_or_fail("dir/open_read_only", open("/data/dir", O_RDONLY));
    check::expect_err("dir/write_read_only", write(ro, b"x"), EBADF);
    let wo = check::unwrap_or_fail("dir/open_write_only", open("/data/dir", O_WRONLY));
    check::expect_err("dir/write", write(wo, b"x"), EISDIR);
    check::ok("dir");
}
