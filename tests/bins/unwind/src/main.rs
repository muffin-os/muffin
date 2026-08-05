#![no_std]
#![no_main]

//! Userspace exerciser for minilib's panic unwinding and backtrace.
//!
//! The final panic is deliberately uncaught. The host test asserts the process
//! exits 101 through minilib's handler, so wrapping it defeats the test.

extern crate alloc;

use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

use minilib::{catch_unwind, exit, write};

fn puts(msg: &str) {
    write(1, msg.as_bytes());
}

static DROPPED: AtomicBool = AtomicBool::new(false);

struct DropGuard;

impl Drop for DropGuard {
    fn drop(&mut self) {
        DROPPED.store(true, Ordering::SeqCst);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    // Annotating the result keeps `Ok` inhabited. Left to inference the closure
    // returns `!`, and the checks below hold vacuously.
    let result: Result<(), _> = catch_unwind(|| {
        let _guard = DropGuard;
        panic!("boom {}", 42);
    });

    let Err(payload) = result else {
        puts("unwind: FAIL result\n");
        exit(1);
    };
    if !DROPPED.load(Ordering::SeqCst) {
        puts("unwind: FAIL drop\n");
        exit(1);
    }
    match payload.downcast_ref::<String>() {
        Some(msg) if msg == "boom 42" => puts("unwind: caught\n"),
        _ => {
            puts("unwind: FAIL payload\n");
            exit(1);
        }
    }

    let nested: Result<(), _> = catch_unwind(|| panic!("again"));
    if nested.is_err() {
        puts("unwind: caught twice\n");
    } else {
        puts("unwind: FAIL nested\n");
        exit(1);
    }

    panic!("escape");
}
