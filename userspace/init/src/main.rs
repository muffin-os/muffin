#![no_std]
#![no_main]

use minilib::{exit, write};

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    let bytes = b"hello from init!\n";
    write(1, bytes);
    foo();
    exit(0);
}

fn foo() {
    bar();
}

fn bar() {
    panic!("I AM CALM");
}
