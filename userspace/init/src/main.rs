#![no_std]
#![no_main]

use minilib::{exit, write};

minilib::entry!(main);

fn main() -> i32 {
    let bytes = b"hello from init!\n";
    let _ = write(1, bytes);
    foo();
    exit(0)
}

fn foo() {
    bar();
}

fn bar() {
    panic!("I AM CALM");
}
