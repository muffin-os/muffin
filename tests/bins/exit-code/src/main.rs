#![no_std]
#![no_main]

use minilib::{exit, write};

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

minilib::entry!(main);

fn main() -> i32 {
    puts("exitcode: exiting 42\n");
    exit(42)
}
