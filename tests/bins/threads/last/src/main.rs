//! The main thread ends with `pthread_exit` while a spawned thread still
//! runs. The process lives until the spawned thread ends, then exits with
//! status 0. The host test asserts the marker order and the exit code.

use std::ffi::c_void;
use std::time::Duration;
use std::{ptr, thread};

unsafe extern "C" {
    fn pthread_exit(result: *mut c_void) -> !;
}

/// The spawned thread runs for this time after the main thread ends.
const WORKER_RUN_TIME: Duration = Duration::from_millis(50);

fn main() {
    // `pthread_exit` never returns, so the handle never detaches the thread.
    let _worker = thread::spawn(|| {
        thread::sleep(WORKER_RUN_TIME);
        println!("threads-last: worker done");
    });
    println!("threads-last: main exits");
    unsafe { pthread_exit(ptr::null_mut()) }
}
