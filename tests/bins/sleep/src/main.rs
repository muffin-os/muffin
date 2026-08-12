#![no_std]
#![no_main]

use minilib::{
    CLOCK_MONOTONIC, EINTR, Errno, Signal, Timespec, clock_gettime, exit, getpid, install_handler,
    kill, nanosleep, write,
};

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

extern "C" fn urgent_handler(_signo: Signal) {}

fn now_ms() -> u64 {
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let _ = clock_gettime(CLOCK_MONOTONIC, &mut ts);
    ts.tv_sec as u64 * 1_000 + ts.tv_nsec as u64 / 1_000_000
}

fn sleep_ms(ms: u64) -> Result<(), Errno> {
    let req = Timespec {
        tv_sec: (ms / 1_000) as i64,
        tv_nsec: ((ms % 1_000) * 1_000_000) as i64,
    };
    nanosleep(&req, None)
}

fn victim() {
    puts("sleep: start\n");

    let before = now_ms();
    let rc = sleep_ms(200);
    let elapsed = now_ms() - before;
    if rc.is_ok() && elapsed >= 200 {
        puts("sleep: duration ok\n");
    } else {
        puts("sleep: UNREACHABLE duration\n");
    }

    // SIGURG defaults to ignore, so the handler is what makes it interrupt
    // the sleep. It must be installed before the driver's kill can land.
    let _ = install_handler(Signal::Urgent, urgent_handler);

    let before = now_ms();
    let rc = sleep_ms(10_000);
    let elapsed = now_ms() - before;
    if rc == Err(EINTR) && elapsed < 8_000 {
        puts("sleep: eintr ok\n");
    } else {
        puts("sleep: UNREACHABLE eintr\n");
    }
}

/// The settle delay must outlast the victim's whole happy path, so the kill
/// can only arrive while the victim sits inside the 10s sleep.
fn driver() {
    let _ = sleep_ms(2_000);
    let _ = kill(1, Signal::Urgent);
}

minilib::entry!(main);

fn main() -> i32 {
    match getpid() {
        1 => victim(),
        2 => driver(),
        _ => puts("sleep: unexpected pid\n"),
    }

    exit(0)
}
