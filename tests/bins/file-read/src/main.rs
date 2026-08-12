#![no_std]
#![no_main]

use minilib::{Stat, Whence, exit, fstat, lseek, open, read, write};

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

const EXPECTED: &[u8] = b"muffin says hi\n";

minilib::entry!(main);

fn main() -> i32 {
    let Ok(fd) = open("/data/hello.txt") else {
        puts("fileio: FAIL\n");
        exit(1);
    };

    let mut buf = [0u8; 64];
    let mut filled = 0usize;
    loop {
        if filled >= buf.len() {
            // More bytes than expected, the file cannot match.
            puts("fileio: FAIL\n");
            exit(1);
        }
        let Ok(n) = read(fd, &mut buf[filled..]) else {
            puts("fileio: FAIL\n");
            exit(1);
        };
        if n == 0 {
            break;
        }
        filled += n;
    }

    if &buf[..filled] != EXPECTED {
        puts("fileio: FAIL\n");
        exit(1);
    }
    puts("fileio: content ok\n");

    let mut stat = Stat::default();
    if fstat(fd, &mut stat).is_err() || stat.size != EXPECTED.len() as u64 {
        puts("fileio: FAIL\n");
        exit(1);
    }

    // The reads above stopped at end of file, so the offset is the file size.
    if lseek(fd, 0, Whence::Cur) != Ok(EXPECTED.len() as u64) {
        puts("fileio: FAIL\n");
        exit(1);
    }

    let mut last = [0u8; 1];
    if lseek(fd, -1, Whence::End) != Ok(EXPECTED.len() as u64 - 1)
        || read(fd, &mut last) != Ok(1)
        || last[0] != b'\n'
    {
        puts("fileio: FAIL\n");
        exit(1);
    }

    let mut word = [0u8; 2];
    if lseek(fd, 12, Whence::Set) != Ok(12) || read(fd, &mut word) != Ok(2) || &word != b"hi" {
        puts("fileio: FAIL\n");
        exit(1);
    }

    // An offset before the start of the file has no byte to land on.
    if lseek(fd, -1, Whence::Set).is_ok() {
        puts("fileio: FAIL\n");
        exit(1);
    }

    puts("fileio: seek ok\n");
    exit(0)
}
