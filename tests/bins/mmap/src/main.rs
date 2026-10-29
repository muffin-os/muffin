#![no_std]
#![no_main]

use minilib::{MapFlags, ProtFlags, getpid, mmap, println};

const PAGE: usize = 4096;
const LEN: usize = 2 * PAGE;
const FLAGS: MapFlags = MapFlags::ANONYMOUS.union(MapFlags::PRIVATE);

minilib::entry!(main);

fn read_write() -> i32 {
    let prot = ProtFlags::READ | ProtFlags::WRITE;

    // A zero-length mapping is invalid and must be rejected.
    if mmap(0, 0, prot, FLAGS, 0, 0).is_ok() {
        println!("mmap: FAIL");
        return 1;
    }

    let Ok(base) = mmap(0, LEN, prot, FLAGS, 0, 0) else {
        println!("mmap: FAIL");
        return 1;
    };

    for i in 0..LEN {
        // Safety: the mapping covers LEN writable bytes starting at base.
        unsafe { core::ptr::write_volatile(base.add(i), (i as u8) ^ 0xA5) };
    }
    for i in 0..LEN {
        // Safety: same mapping, reading back the bytes just written.
        let got = unsafe { core::ptr::read_volatile(base.add(i)) };
        if got != (i as u8) ^ 0xA5 {
            println!("mmap: FAIL");
            return 1;
        }
    }

    println!("mmap: ok");
    0
}

// Reads a `PROT_READ` page, then writes to it, which must kill the process.
fn read_only() -> i32 {
    let Ok(base) = mmap(0, PAGE, ProtFlags::READ, FLAGS, 0, 0) else {
        println!("mmap: FAIL");
        return 1;
    };

    // Safety: the mapping covers PAGE readable bytes starting at base.
    if unsafe { core::ptr::read_volatile(base) } != 0 {
        println!("mmap: FAIL");
        return 1;
    }

    println!("mmap: readonly write next");
    // Safety: this write is meant to fault. If the process survives it, the
    // host test fails.
    unsafe { core::ptr::write_volatile(base, 1) };

    println!("mmap: readonly write survived");
    1
}

fn main() -> i32 {
    match getpid() {
        2 => read_write(),
        3 => read_only(),
        _ => {
            println!("mmap: unexpected pid");
            1
        }
    }
}
