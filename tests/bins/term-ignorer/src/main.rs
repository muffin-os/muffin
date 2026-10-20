#![no_std]
#![no_main]

use core::time::Duration;

use minilib::{
    SaFlags, SigAction, SigHandler, SigSet, Signal, exit, getpid, nanosleep, println, shutdown,
    sigaction,
};

fn victim() -> ! {
    // A restorer of 0 is sound only because an ignored signal never enters a
    // handler, so nothing ever returns through the restorer trampoline.
    let action = SigAction {
        handler: SigHandler::IGNORE,
        mask: SigSet::EMPTY,
        flags: SaFlags::default(),
        restorer: 0,
    };
    if let Err(e) = sigaction(Signal::Terminate, Some(&action), None) {
        println!("term-ignorer: FAIL sigaction {e}");
        exit(1);
    }

    println!("term-ignorer: ignoring sigterm");

    loop {
        let _ = nanosleep(&Duration::from_secs(1).into(), None);
    }
}

fn driver() -> ! {
    // Outlasts the victim's sigaction, so the shutdown's SIGTERM broadcast never
    // finds the default action.
    let _ = nanosleep(&Duration::from_secs(2).into(), None);
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
