#![no_std]
#![no_main]

use minilib::{Stat, Whence, fstat, lseek, open, println, read};

const EXPECTED: &[u8] = b"muffin says hi\n";

minilib::entry!(main);

fn main() -> i32 {
    let Ok(fd) = open("/data/hello.txt") else {
        println!("fileio: FAIL");
        return 1;
    };

    let mut buf = [0u8; 64];
    let mut filled = 0usize;
    loop {
        if filled >= buf.len() {
            // More bytes than expected, the file cannot match.
            println!("fileio: FAIL");
            return 1;
        }
        let Ok(n) = read(fd, &mut buf[filled..]) else {
            println!("fileio: FAIL");
            return 1;
        };
        if n == 0 {
            break;
        }
        filled += n;
    }

    if &buf[..filled] != EXPECTED {
        println!("fileio: FAIL");
        return 1;
    }
    println!("fileio: content ok");

    let mut stat = Stat::default();
    if fstat(fd, &mut stat).is_err() || stat.size != EXPECTED.len() as u64 {
        println!("fileio: FAIL");
        return 1;
    }

    // The reads above stopped at end of file, so the offset is the file size.
    if lseek(fd, 0, Whence::Cur) != Ok(EXPECTED.len() as u64) {
        println!("fileio: FAIL");
        return 1;
    }

    let mut last = [0u8; 1];
    if lseek(fd, -1, Whence::End) != Ok(EXPECTED.len() as u64 - 1)
        || read(fd, &mut last) != Ok(1)
        || last[0] != b'\n'
    {
        println!("fileio: FAIL");
        return 1;
    }

    let mut word = [0u8; 2];
    if lseek(fd, 12, Whence::Set) != Ok(12) || read(fd, &mut word) != Ok(2) || &word != b"hi" {
        println!("fileio: FAIL");
        return 1;
    }

    // An offset before the start of the file has no byte to land on.
    if lseek(fd, -1, Whence::Set).is_ok() {
        println!("fileio: FAIL");
        return 1;
    }

    println!("fileio: seek ok");
    0
}
