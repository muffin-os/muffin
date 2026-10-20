#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use minilib::{Signal, getpid, install_handler, nanosleep, println};

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn term_handler(_signo: Signal) {
    println!("term-sleeper: sigterm received");
    TERMINATED.store(true, Ordering::SeqCst);
}

fn victim() -> i32 {
    let _ = install_handler(Signal::Terminate, term_handler);
    // Sleeping keeps the victim inside the kernel, so only the syscall return
    // path can deliver the handler. Spinning would move delivery to the tick.
    while !TERMINATED.load(Ordering::SeqCst) {
        let _ = nanosleep(&Duration::from_secs(10).into(), None);
    }
    0
}

fn driver() -> ! {
    // Outlasts the victim's sigaction, so the shutdown's SIGTERM broadcast never
    // finds the default action. A ping/ack handshake cannot replace it, because
    // the victim must already be parked in nanosleep when SIGTERM lands, and
    // it cannot send an ack from inside that sleep.
    let _ = nanosleep(&Duration::from_secs(2).into(), None);
    minilib::shutdown()
}

minilib::entry!(main);

fn main() -> i32 {
    match getpid() {
        1 => victim(),
        2 => driver(),
        _ => {
            println!("term-sleeper: unexpected pid");
            1
        }
    }
}
