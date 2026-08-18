#![no_std]
#![no_main]

use minilib::println;

minilib::entry!(main);

fn main() -> i32 {
    println!("hello from init!");
    0
}
