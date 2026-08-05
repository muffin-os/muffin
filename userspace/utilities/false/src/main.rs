#![no_std]
#![no_main]

use minilib::exit;

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    exit(1);
}
