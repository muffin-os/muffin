//! Powers the machine off after a delay, so a test can watch the service
//! manager restart other services first.

use std::thread;
use std::time::Duration;

fn main() {
    thread::sleep(Duration::from_secs(3));
    muffin_sys::poweroff()
}
