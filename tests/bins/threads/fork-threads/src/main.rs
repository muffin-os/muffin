//! A fork child owns only the per-thread blocks of the calling thread. The
//! child reads the TLS block of a sibling thread of the parent. The read
//! must fault, and the `SIGSEGV` handler exits the child with 42.

use std::ffi::c_int;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use std::{ptr, thread};

/// The child exits with this code when the read of the sibling TLS block faults.
const FAULTED: c_int = 42;

/// This is the thread pointer (`%fs:0`) of the sibling thread.
static SIBLING_TP: AtomicUsize = AtomicUsize::new(0);

static PARK: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

extern "C" fn on_segv(_: c_int) {
    // SAFETY: `_exit` is async-signal-safe.
    unsafe { libc::_exit(FAULTED) }
}

fn sibling() {
    let tp: usize;
    // SAFETY: `%fs:0` holds the thread pointer on x86_64.
    unsafe { core::arch::asm!("mov {}, fs:0", out(reg) tp) };
    SIBLING_TP.store(tp, Ordering::Release);
    let (lock, wake) = &PARK;
    let mut guard = lock.lock().expect("park mutex is poisoned");
    loop {
        guard = wake.wait(guard).expect("park mutex is poisoned");
    }
}

fn main() {
    thread::spawn(sibling);
    let tp = loop {
        match SIBLING_TP.load(Ordering::Acquire) {
            0 => thread::sleep(Duration::from_millis(1)),
            tp => break tp,
        }
    };
    let handler = on_segv as extern "C" fn(c_int);
    // SAFETY: `on_segv` is a valid handler that calls only `_exit`.
    unsafe { libc::signal(libc::SIGSEGV, handler as libc::sighandler_t) };

    // SAFETY: the child calls only `read_volatile` and `_exit`.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // SAFETY: this test deliberately reads an invalid address to trigger a page fault.
        // The child does not own the TLS block of the sibling thread.
        black_box(unsafe { ptr::with_exposed_provenance::<u8>(tp).read_volatile() });
        // SAFETY: `_exit` does not return.
        unsafe { libc::_exit(0) };
    }
    let mut status = 0;
    // SAFETY: `status` is a live local.
    let waited = unsafe { libc::waitpid(pid, &raw mut status, 0) };
    if pid > 0 && waited == pid && libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == FAULTED {
        println!("fork-threads: sibling tls unmapped ok");
    } else {
        println!("fork-threads: FAIL status={status}");
    }
    std::process::exit(0);
}
