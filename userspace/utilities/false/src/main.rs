#![no_std]
#![no_main]

use minilib::exit;

minilib::entry!(main);

fn main() -> i32 {
    exit(1)
}
