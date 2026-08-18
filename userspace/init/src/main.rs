#![no_std]
#![no_main]

use minilib::println;

minilib::entry!(main);

fn main() -> i32 {
    println!("hello from init!");
    foo();
    0
}

fn foo() {
    bar();
}

fn bar() {
    panic!("I AM CALM");
}
