//! A spawned thread ends the process with `exit(7)` while the main thread
//! blocks in `join`. The host test asserts the exit code 7.

use std::time::Duration;
use std::{process, thread};

/// The main thread needs this time to reach `join`. The exit code is 7 in both
/// orders. The pause makes the test exit the process while a sibling thread waits.
const JOIN_DELAY: Duration = Duration::from_millis(50);

fn exit_from_spawned_thread() {
    thread::sleep(JOIN_DELAY);
    println!("threads-exit: worker exits");
    process::exit(7)
}

fn main() {
    let worker = thread::spawn(exit_from_spawned_thread);
    let _ = worker.join();
    println!("threads-exit: FAIL join returned");
    process::exit(1);
}
