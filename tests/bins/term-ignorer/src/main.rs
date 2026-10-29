#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use minilib::{
    SaFlags, SigAction, SigHandler, SigSet, Signal, exit, getpid, install_handler, kill, nanosleep,
    println, shutdown, sigaction,
};

/// Spawn order fixes the pid, so the pid picks the role. pid 1 is test-init.
#[derive(Clone, Copy)]
enum Role {
    Victim = 2,
    Driver = 3,
}

impl Role {
    fn of(pid: i64) -> Option<Self> {
        [Self::Victim, Self::Driver]
            .into_iter()
            .find(|role| role.pid() == pid)
    }

    fn pid(self) -> i64 {
        self as i64
    }
}

/// Driver to victim ping, and the victim's reply. Both default to `Ignore`,
/// so one that lands before its handler is armed is dropped, never fatal.
const PING: Signal = Signal::WindowChanged;
const ACK: Signal = Signal::Urgent;

static ACKED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_ack(_signo: Signal) {
    ACKED.store(true, Ordering::SeqCst);
}

/// Armed only after the SIGTERM ignore is in place, so the reply proves it.
extern "C" fn on_ping(_signo: Signal) {
    let _ = kill(Role::Driver.pid(), ACK);
}

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
    if let Err(e) = install_handler(PING, on_ping) {
        println!("term-ignorer: FAIL install_handler {e}");
        exit(1);
    }

    println!("term-ignorer: ignoring sigterm");

    loop {
        let _ = nanosleep(&Duration::from_secs(1).into(), None);
    }
}

fn driver() -> ! {
    if let Err(e) = install_handler(ACK, on_ack) {
        println!("term-ignorer: FAIL install_handler {e}");
        exit(1);
    }
    // A ping sent before the victim armed its handler is dropped, so keep
    // pinging until a reply comes back.
    while !ACKED.load(Ordering::SeqCst) {
        if let Err(e) = kill(Role::Victim.pid(), PING) {
            println!("term-ignorer: FAIL kill {e}");
            exit(1);
        }
        let _ = nanosleep(&Duration::from_millis(50).into(), None);
    }
    shutdown()
}

minilib::entry!(main);

fn main() -> i32 {
    match Role::of(getpid()) {
        Some(Role::Victim) => victim(),
        Some(Role::Driver) => driver(),
        None => {
            println!("term-ignorer: unexpected pid");
            1
        }
    }
}
