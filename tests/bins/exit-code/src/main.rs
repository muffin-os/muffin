#![no_std]
#![no_main]

use minilib::{exit, write};

fn puts(msg: &str) {
    write(1, msg.as_bytes());
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    puts("exitcode: exiting 42\n");
    exit(42);
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &::core::panic::PanicInfo) -> ! {
    loop {}
}
