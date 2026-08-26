#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use minilib::{Errno, Signal, Timespec, getpid, install_handler, nanosleep, println};

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn term_handler(_signo: Signal) {
    println!("term-sleeper: sigterm received");
    TERMINATED.store(true, Ordering::SeqCst);
}

fn sleep_ms(ms: u64) -> Result<(), Errno> {
    let req = Timespec {
        tv_sec: (ms / 1_000) as i64,
        tv_nsec: ((ms % 1_000) * 1_000_000) as i64,
    };
    nanosleep(&req, None)
}

/// Sleeping keeps the victim inside the kernel, so a user-mode timer tick can
/// never deliver the handler. A spinning wait here would silently stop
/// covering the syscall return path.
fn victim() -> i32 {
    let _ = install_handler(Signal::Terminate, term_handler);
    while !TERMINATED.load(Ordering::SeqCst) {
        let _ = sleep_ms(10_000);
    }
    0
}

/// The settle delay must outlast handler installation, so the kernel's SIGTERM
/// broadcast can never beat the victim's sigaction and kill it by the default
/// action.
fn driver() -> ! {
    let _ = sleep_ms(2_000);
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
