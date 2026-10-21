#![no_std]
#![no_main]

mod check;

use minilib::{
    EBADF, EEXIST, EISDIR, ENAMETOOLONG, ENOENT, O_APPEND, O_CREAT, O_EXCL, O_RDONLY, O_RDWR,
    O_WRONLY, Stat, Whence, fstat, lseek, open, write,
};

const SEED: &[u8] = b"muffin seed data\n";

const SUFFIX: &[u8] = b"appended\n";

const NEW: &[u8] = b"created by file-write\n";

const CHUNK: usize = 4096;

// 300 blocks of 1 KiB: past the 12 direct and the 256 single-indirect slots
const BIG_LEN: usize = 300 * 1024;

const SPARSE_OFFSET: usize = 70 << 20;

const HOLE_OFFSET: usize = 60 << 20;

minilib::entry!(main);

fn main() -> i32 {
    overwrite();
    append();
    dir();
    create();
    excl();
    big();
    0
}

// same formula as the host-side check in tests/file_write.rs
fn pattern(buf: &mut [u8], offset: usize) {
    for (i, byte) in buf.iter_mut().enumerate() {
        let p = offset + i;
        *byte = (p ^ (p >> 10) ^ (p >> 18)) as u8;
    }
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

fn create() {
    let fd = check::unwrap_or_fail("create/open", open("/data/new.txt", O_CREAT | O_WRONLY));
    check::expect_ok("create/write", write(fd, NEW), NEW.len());
    check::require(
        "create/fstat_size",
        size_of("create/fstat", fd) == NEW.len() as u64,
    );

    let ro = check::unwrap_or_fail("create/reopen", open("/data/new.txt", O_RDONLY));
    let mut buf = [0_u8; NEW.len() + 1];
    let n = check::read_to_end("create/read", ro, &mut buf);
    check::require("create/contents", &buf[..n] == NEW);
    check::ok("create");
}

fn excl() {
    check::expect_err(
        "excl/existing",
        open("/data/seed.txt", O_CREAT | O_EXCL | O_WRONLY),
        EEXIST,
    );
    check::expect_err(
        "excl/missing_dir",
        open("/data/missing/x", O_CREAT | O_WRONLY),
        ENOENT,
    );
    check::expect_err(
        "excl/through_file",
        open("/data/seed.txt/x", O_CREAT | O_WRONLY),
        ENOENT,
    );

    // one byte past EXT2_NAME_LEN
    let mut long = [b'a'; "/data/".len() + 256];
    long[.."/data/".len()].copy_from_slice(b"/data/");
    let Ok(long) = core::str::from_utf8(&long) else {
        check::fail("excl/long_name_utf8")
    };
    check::expect_err(
        "excl/name_too_long",
        open(long, O_CREAT | O_WRONLY),
        ENAMETOOLONG,
    );
    check::ok("excl");
}

fn big() {
    let fd = check::unwrap_or_fail("big/open", open("/data/big.bin", O_CREAT | O_WRONLY));
    let mut chunk = [0_u8; CHUNK];
    for offset in (0..BIG_LEN).step_by(CHUNK) {
        pattern(&mut chunk, offset);
        check::expect_ok("big/write", write(fd, &chunk), CHUNK);
    }
    check::require("big/fstat_size", size_of("big/fstat", fd) == BIG_LEN as u64);

    let ro = check::unwrap_or_fail("big/reopen", open("/data/big.bin", O_RDONLY));
    let mut expected = [0_u8; CHUNK];
    for offset in (0..BIG_LEN).step_by(CHUNK) {
        check::require(
            "big/read",
            check::read_to_end("big/read", ro, &mut chunk) == CHUNK,
        );
        pattern(&mut expected, offset);
        check::require("big/contents", chunk == expected);
    }
    check::expect_ok("big/read_eof", minilib::read(ro, &mut chunk), 0);

    let sparse = check::unwrap_or_fail(
        "big/open_sparse",
        open("/data/sparse.bin", O_CREAT | O_RDWR),
    );
    check::expect_ok(
        "big/lseek_sparse",
        lseek(sparse, SPARSE_OFFSET as i64, Whence::Set),
        SPARSE_OFFSET as u64,
    );
    pattern(&mut chunk[..1024], SPARSE_OFFSET);
    check::expect_ok("big/write_sparse", write(sparse, &chunk[..1024]), 1024);
    check::require(
        "big/fstat_sparse_size",
        size_of("big/fstat_sparse", sparse) == (SPARSE_OFFSET + 1024) as u64,
    );
    check::expect_ok(
        "big/lseek_hole",
        lseek(sparse, HOLE_OFFSET as i64, Whence::Set),
        HOLE_OFFSET as u64,
    );
    check::require(
        "big/read_hole",
        check::read_to_end("big/read_hole", sparse, &mut chunk[..1024]) == 1024,
    );
    check::require("big/hole_is_zero", chunk[..1024].iter().all(|&b| b == 0));
    check::ok("big");
}
