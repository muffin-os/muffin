#![no_std]
#![no_main]

use minilib::{MapFlags, ProtFlags, exit, mmap, write};

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

const PAGE: usize = 4096;
const LEN: usize = 2 * PAGE;

minilib::entry!(main);

fn main() -> i32 {
    let prot = ProtFlags::READ | ProtFlags::WRITE;
    let flags = MapFlags::ANONYMOUS | MapFlags::PRIVATE;

    // A zero-length mapping is invalid and must be rejected.
    if mmap(0, 0, prot, flags, 0, 0).is_ok() {
        puts("mmap: FAIL\n");
        exit(1);
    }

    let Ok(base) = mmap(0, LEN, prot, flags, 0, 0) else {
        puts("mmap: FAIL\n");
        exit(1);
    };

    for i in 0..LEN {
        // Safety: the mapping covers LEN writable bytes starting at base.
        unsafe { core::ptr::write_volatile(base.add(i), (i as u8) ^ 0xA5) };
    }
    for i in 0..LEN {
        // Safety: same mapping, reading back the bytes just written.
        let got = unsafe { core::ptr::read_volatile(base.add(i)) };
        if got != (i as u8) ^ 0xA5 {
            puts("mmap: FAIL\n");
            exit(1);
        }
    }

    puts("mmap: ok\n");
    exit(0)
}
