#![no_std]
#![no_main]

use minilib::{exit, write};

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    let bytes = b"hello from init!\n";
    write(1, bytes);
    exit();
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &::core::panic::PanicInfo) -> ! {
    loop {}
}
