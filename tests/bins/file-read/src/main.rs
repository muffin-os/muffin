#![no_std]
#![no_main]

use minilib::{Stat, Whence, exit, fstat, lseek, open, read, write};

fn puts(msg: &str) {
    write(1, msg.as_bytes());
}

const EXPECTED: &[u8] = b"muffin says hi\n";

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    let fd = open("/data/hello.txt");
    if fd < 0 {
        puts("fileio: FAIL\n");
        exit(1);
    }

    let mut buf = [0u8; 64];
    let mut filled = 0usize;
    loop {
        if filled >= buf.len() {
            // More bytes than expected, the file cannot match.
            puts("fileio: FAIL\n");
            exit(1);
        }
        let n = read(fd, &mut buf[filled..]);
        if n < 0 {
            puts("fileio: FAIL\n");
            exit(1);
        }
        if n == 0 {
            break;
        }
        filled += n as usize;
    }

    if &buf[..filled] != EXPECTED {
        puts("fileio: FAIL\n");
        exit(1);
    }
    puts("fileio: content ok\n");

    let mut stat = Stat::default();
    if fstat(fd, &mut stat) < 0 || stat.size != EXPECTED.len() as u64 {
        puts("fileio: FAIL\n");
        exit(1);
    }

    // The reads above stopped at end of file, so the offset is the file size.
    if lseek(fd, 0, Whence::Cur) != EXPECTED.len() as i64 {
        puts("fileio: FAIL\n");
        exit(1);
    }

    let mut last = [0u8; 1];
    if lseek(fd, -1, Whence::End) != EXPECTED.len() as i64 - 1
        || read(fd, &mut last) != 1
        || last[0] != b'\n'
    {
        puts("fileio: FAIL\n");
        exit(1);
    }

    let mut word = [0u8; 2];
    if lseek(fd, 12, Whence::Set) != 12 || read(fd, &mut word) != 2 || &word != b"hi" {
        puts("fileio: FAIL\n");
        exit(1);
    }

    // An offset before the start of the file has no byte to land on.
    if lseek(fd, -1, Whence::Set) >= 0 {
        puts("fileio: FAIL\n");
        exit(1);
    }

    puts("fileio: seek ok\n");
    exit(0);
}
