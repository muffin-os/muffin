#![no_std]
#![no_main]

use minilib::println;

minilib::entry!(main);

fn main() -> i32 {
    println!("exitcode: exiting 42");
    42
}
