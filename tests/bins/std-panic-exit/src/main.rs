//! The panic is deliberately uncaught; the host test asserts the exit code.

fn main() {
    panic!("std-panic-exit: escape");
}
