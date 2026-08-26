#![no_std]
#![no_main]

use minilib::{
    Errno, SaFlags, SigAction, SigHandler, Signal, Timespec, getpid, nanosleep, println, shutdown,
    sigaction,
};

fn sleep_ms(ms: u64) -> Result<(), Errno> {
    let req = Timespec {
        tv_sec: (ms / 1_000) as i64,
        tv_nsec: ((ms % 1_000) * 1_000_000) as i64,
    };
    nanosleep(&req, None)
}

fn victim() -> ! {
    // A restorer of 0 is sound only because an ignored signal never enters a
    // handler, so nothing ever returns through the restorer trampoline.
    let action = SigAction {
        handler: SigHandler::IGNORE,
        mask: 0,
        flags: SaFlags::default(),
        restorer: 0,
    };
    let _ = sigaction(Signal::Terminate, Some(&action), None);

    println!("term-ignorer: ignoring sigterm");

    loop {
        let _ = sleep_ms(1_000);
    }
}

/// The settle delay must outlast the victim installing the ignore disposition,
/// otherwise the sweep could find a process still using the default action.
fn driver() -> ! {
    let _ = sleep_ms(2_000);
    shutdown()
}

minilib::entry!(main);

fn main() -> i32 {
    match getpid() {
        1 => victim(),
        2 => driver(),
        _ => {
            println!("term-ignorer: unexpected pid");
            1
        }
    }
}
