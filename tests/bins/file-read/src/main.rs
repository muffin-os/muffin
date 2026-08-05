#![no_std]
#![no_main]

use minilib::{exit, open, read, write};

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

    if &buf[..filled] == EXPECTED {
        puts("fileio: content ok\n");
        exit(0);
    }
    puts("fileio: FAIL\n");
    exit(1);
}
