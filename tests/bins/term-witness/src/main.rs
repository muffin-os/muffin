#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use minilib::{Errno, Signal, Timespec, getpid, install_handler, nanosleep, println};

// Same spin as `PUMP_SPIN` in signals-init, which documents it.
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

fn victim() -> i32 {
    let _ = install_handler(Signal::Terminate, term_handler);

    // Spinning keeps the victim in user mode, so only a timer tick with a ring 3
    // frame can deliver the handler. Sleeping would move delivery to the syscall
    // return path.
    loop {
        busy_delay_n(PUMP_SPIN);
        if TERMINATED.load(Ordering::SeqCst) {
            return 0;
        }
    }
}

fn driver() -> ! {
    // Outlasts the victim's sigaction, so the shutdown's SIGTERM broadcast never
    // finds the default action.
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
