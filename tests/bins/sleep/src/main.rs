#![no_std]
#![no_main]

use core::time::Duration;

use minilib::{
    ClockId, EINTR, Signal, Timespec, clock_gettime, exit, getpid, install_handler, kill,
    nanosleep, println,
};

/// Spawn order fixes the pid, so the pid picks the role.
#[derive(Clone, Copy)]
enum Role {
    Victim = 1,
    Driver = 2,
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

extern "C" fn urgent_handler(_signo: Signal) {}

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

    // SIGURG defaults to ignore, so the handler is what makes it interrupt
    // the sleep. It must be installed before the driver's kill can land.
    let _ = install_handler(Signal::Urgent, urgent_handler);

    let before = now();
    let rc = nanosleep(&Duration::from_secs(10).into(), None);
    let elapsed = now().saturating_sub(before);
    if rc == Err(EINTR) && elapsed < Duration::from_secs(8) {
        println!("sleep: eintr ok");
    } else {
        println!("sleep: UNREACHABLE eintr");
    }
}

fn driver() {
    // Outlasts the victim's whole happy path, so the kill can only land inside
    // its 10s sleep.
    let _ = nanosleep(&Duration::from_secs(2).into(), None);
    let _ = kill(Role::Victim.pid(), Signal::Urgent);
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
