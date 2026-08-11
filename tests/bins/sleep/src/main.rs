#![no_std]
#![no_main]

use core::ffi::c_int;

use minilib::{
    CLOCK_MONOTONIC, EINTR, Signal, Timespec, clock_gettime, exit, getpid, install_handler, kill,
    nanosleep, write,
};

fn puts(msg: &str) {
    write(1, msg.as_bytes());
}

extern "C" fn urgent_handler(_signo: Signal) {}

fn now_ms() -> u64 {
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    clock_gettime(CLOCK_MONOTONIC, &mut ts);
    ts.tv_sec as u64 * 1_000 + ts.tv_nsec as u64 / 1_000_000
}

fn sleep_ms(ms: u64) -> c_int {
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
    if rc == 0 && elapsed >= 200 {
        puts("sleep: duration ok\n");
    } else {
        puts("sleep: UNREACHABLE duration\n");
    }

    // SIGURG defaults to ignore, so the handler is what makes it interrupt
    // the sleep. It must be installed before the driver's kill can land.
    install_handler(Signal::Urgent, urgent_handler);

    let before = now_ms();
    let rc = sleep_ms(10_000);
    let elapsed = now_ms() - before;
    if rc == -c_int::from(EINTR) && elapsed < 8_000 {
        puts("sleep: eintr ok\n");
    } else {
        puts("sleep: UNREACHABLE eintr\n");
    }
}

/// The settle delay must outlast the victim's whole happy path, so the kill
/// can only arrive while the victim sits inside the 10s sleep.
fn driver() {
    sleep_ms(2_000);
    kill(1, Signal::Urgent);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    match getpid() {
        1 => victim(),
        2 => driver(),
        _ => puts("sleep: unexpected pid\n"),
    }

    exit(0);
}
