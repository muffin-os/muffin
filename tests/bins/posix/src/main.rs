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
    minilib::println!("posix: start");

    fd::run();
    mem::run();
    process::run();
    signal::run();
    time::run();
    exec::run();

    minilib::println!("posix: all checks passed");
    0
}
