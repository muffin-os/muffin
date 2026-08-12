#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;

use minilib::{CLOCK_MONOTONIC, Stat, Timespec, clock_gettime, exit, fstat, open, read, write};

const PATH: &str = "/var/large.bin";
const CHUNK_LEN: usize = 64 * 1024;
const MIB: u64 = 1024 * 1024;

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

fn put_u64(n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    let mut value = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let _ = write(1, &buf[i..]);
}

fn now_us() -> u64 {
    let mut tp = Timespec::default();
    let _ = clock_gettime(CLOCK_MONOTONIC, &mut tp);
    (tp.tv_sec as u64) * 1_000_000 + (tp.tv_nsec as u64) / 1000
}

fn fail(what: &str) -> ! {
    puts("sandbox: FAIL ");
    puts(what);
    puts("\n");
    exit(1)
}

minilib::entry!(main);

fn main() -> i32 {
    let Ok(fd) = open(PATH) else {
        fail("open");
    };

    let mut stat = Stat::default();
    if fstat(fd, &mut stat).is_err() {
        fail("fstat");
    }

    let mut buf = vec![0u8; CHUNK_LEN];
    let start_us = now_us();
    let mut total = 0_u64;
    while total < stat.size {
        let Ok(n) = read(fd, &mut buf) else {
            fail("read");
        };
        if n == 0 {
            break;
        }
        total += n as u64;
    }
    let elapsed_us = now_us().saturating_sub(start_us).max(1);

    let tenth_mib_per_s = total * 10 * 1_000_000 / elapsed_us / MIB;
    puts("sandbox: read ");
    put_u64(total);
    puts(" bytes in ");
    put_u64(elapsed_us / 1000);
    puts("ms at ");
    put_u64(tenth_mib_per_s / 10);
    puts(".");
    put_u64(tenth_mib_per_s % 10);
    puts(" MiB/s\n");

    exit(0)
}
