#![no_std]
#![no_main]

extern crate alloc;

mod check;
mod exec;
mod fd;
mod mem;
mod process;
mod signal;
mod time;

minilib::entry!(main);

fn main() -> i32 {
    check::puts("posix: start\n");

    fd::run();
    mem::run();
    process::run();
    signal::run();
    time::run();
    exec::run();

    check::puts("posix: all checks passed\n");
    0
}
