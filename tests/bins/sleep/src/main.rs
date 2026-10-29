#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use minilib::{
    ClockId, EINTR, Signal, Timespec, clock_gettime, exit, getpid, install_handler, kill,
    nanosleep, println,
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

/// The driver's ping and the victim's reply. Both default to `Ignore`, so one
/// that lands before its handler is armed is dropped, never fatal, and an
/// unhandled ping cannot cut the happy-path sleep short.
const PING: Signal = Signal::Urgent;
const ACK: Signal = Signal::WindowChanged;

static ACKED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_ack(_signo: Signal) {
    ACKED.store(true, Ordering::SeqCst);
}

extern "C" fn on_ping(_signo: Signal) {}

fn now() -> Duration {
    let mut ts = Timespec::default();
    let Ok(now) = clock_gettime(ClockId::Monotonic, &mut ts).and_then(|()| Duration::try_from(ts))
    else {
        println!("sleep: UNREACHABLE clock");
        exit(1);
    };
    now
}

fn victim() {
    println!("sleep: start");

    let before = now();
    let rc = nanosleep(&Duration::from_millis(200).into(), None);
    let elapsed = now().saturating_sub(before);
    if rc.is_ok() && elapsed >= Duration::from_millis(200) {
        println!("sleep: duration ok");
    } else {
        println!("sleep: UNREACHABLE duration");
    }

    // The ping defaults to ignore, so the handler is what makes it interrupt
    // the sleep.
    if let Err(e) = install_handler(PING, on_ping) {
        println!("sleep: FAIL install_handler {e}");
        return;
    }

    let before = now();
    let rc = nanosleep(&Duration::from_secs(10).into(), None);
    let elapsed = now().saturating_sub(before);
    if rc == Err(EINTR) && elapsed < Duration::from_secs(8) {
        println!("sleep: eintr ok");
    } else {
        println!("sleep: UNREACHABLE eintr");
    }
    // Replying only after the sleep was cut short means a ping that landed
    // before the sleep began is simply followed by another.
    if let Err(e) = kill(Role::Driver.pid(), ACK) {
        println!("sleep: FAIL kill {e}");
    }
}

fn driver() {
    if let Err(e) = install_handler(ACK, on_ack) {
        println!("sleep: FAIL install_handler {e}");
        return;
    }
    while !ACKED.load(Ordering::SeqCst) {
        if let Err(e) = kill(Role::Victim.pid(), PING) {
            println!("sleep: FAIL kill {e}");
            return;
        }
        let _ = nanosleep(&Duration::from_millis(50).into(), None);
    }
}

minilib::entry!(main);

fn main() -> i32 {
    match Role::of(getpid()) {
        Some(Role::Victim) => victim(),
        Some(Role::Driver) => driver(),
        None => println!("sleep: unexpected pid"),
    }

    0
}
