#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use minilib::{Errno, Signal, Timespec, getpid, install_handler, nanosleep, println};

/// Roughly 14ms of spin per pump, same shape as signals-init.
const PUMP_SPIN: u64 = 2_000_000;

fn busy_delay_n(n: u64) {
    let mut counter: u64 = 0;
    while counter < n {
        counter = unsafe { core::ptr::read_volatile(&counter) } + 1;
    }
}

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn term_handler(_signo: Signal) {
    println!("term-witness: sigterm received");
    TERMINATED.store(true, Ordering::SeqCst);
}

fn sleep_ms(ms: u64) -> Result<(), Errno> {
    let req = Timespec {
        tv_sec: (ms / 1_000) as i64,
        tv_nsec: ((ms % 1_000) * 1_000_000) as i64,
    };
    nanosleep(&req, None)
}

/// Handlers are delivered only at timer ticks that land in user mode. Waiting
/// in a nanosleep retry loop starves delivery, because a pending signal makes
/// every re-entered sleep fail EINTR immediately and the process then spends
/// almost all its time inside the kernel. The wait must spin in user mode.
fn victim() -> i32 {
    let _ = install_handler(Signal::Terminate, term_handler);

    loop {
        busy_delay_n(PUMP_SPIN);
        if TERMINATED.load(Ordering::SeqCst) {
            return 0;
        }
    }
}

/// The settle delay must outlast handler installation, so the kernel's SIGTERM
/// broadcast can never beat the victim's sigaction and kill it by default
/// action instead.
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
            println!("term-witness: unexpected pid");
            1
        }
    }
}
