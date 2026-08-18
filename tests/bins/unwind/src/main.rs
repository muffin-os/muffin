#![no_std]
#![no_main]

//! Userspace exerciser for minilib's panic unwinding and backtrace.
//!
//! The final panic is deliberately uncaught. The host test asserts the process
//! exits 101 through minilib's handler, so wrapping it defeats the test.

extern crate alloc;

use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

use minilib::{catch_unwind, println};

static DROPPED: AtomicBool = AtomicBool::new(false);

struct DropGuard;

impl Drop for DropGuard {
    fn drop(&mut self) {
        DROPPED.store(true, Ordering::SeqCst);
    }
}

minilib::entry!(main);

fn main() -> i32 {
    // Annotating the result keeps `Ok` inhabited. Left to inference the closure
    // returns `!`, and the checks below hold vacuously.
    let result: Result<(), _> = catch_unwind(|| {
        let _guard = DropGuard;
        panic!("boom {}", 42);
    });

    let Err(payload) = result else {
        println!("unwind: FAIL result");
        return 1;
    };
    if !DROPPED.load(Ordering::SeqCst) {
        println!("unwind: FAIL drop");
        return 1;
    }
    match payload.downcast_ref::<String>() {
        Some(msg) if msg == "boom 42" => println!("unwind: caught"),
        _ => {
            println!("unwind: FAIL payload");
            return 1;
        }
    }

    let nested: Result<(), _> = catch_unwind(|| panic!("again"));
    if nested.is_err() {
        println!("unwind: caught twice");
    } else {
        println!("unwind: FAIL nested");
        return 1;
    }

    panic!("escape")
}
