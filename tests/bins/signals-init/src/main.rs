#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use minilib::{
    SigMaskHow, SigSet, Signal, exit, getpid, install_handler, kill, sigpending, sigprocmask,
    syscall3, write,
};

fn puts(msg: &str) {
    write(1, msg.as_bytes());
}

fn busy_delay_n(n: u64) {
    let mut counter: u64 = 0;
    while counter < n {
        counter = unsafe { core::ptr::read_volatile(&counter) } + 1;
    }
}

/// Driver to victim ping, and the victim's reply. Both default to `Ignore`
/// rather than `Terminate`, so a handshake signal that arrives before its
/// handler is armed can never kill the peer. The pair is also disjoint from
/// `Signal::Usr1`, which the driver raises on itself in the self test, so the
/// two exchanges cannot be confused for one another.
const PING: Signal = Signal::WindowChanged;
const ACK: Signal = Signal::Urgent;

/// Roughly 14ms of spin per pump. Small enough that pumping does not flood the
/// serial transcript with syscall TRACE lines, large enough that a handful of
/// pumps still covers process load and stop/cont settling.
const PUMP_SPIN: u64 = 2_000_000;

/// Signal handlers are delivered only at syscall exit. A loop that spins purely
/// in userspace never runs a pending handler. The `getpid` here is a cheap side
/// effect free syscall whose exit path drains any pending delivery.
fn pump() {
    busy_delay_n(PUMP_SPIN);
    let _ = getpid();
}

/// Pump up to `budget` times, returning early the moment the ack counter has
/// reached `target`. The counter is checked before the first pump because the
/// ack may already have landed during an earlier phase.
fn wait_acks(target: u32, budget: u32) -> bool {
    if acks() >= target {
        return true;
    }
    let mut i = 0;
    while i < budget {
        pump();
        if acks() >= target {
            return true;
        }
        i += 1;
    }
    acks() >= target
}

/// Pump `budget` times with no early exit, used for the negative checks where
/// we want to give a signal the chance to (wrongly) be acked and then confirm
/// it was not.
fn spin(budget: u32) {
    let mut i = 0;
    while i < budget {
        pump();
        i += 1;
    }
}

/// Monotonic count of acks the driver has received from the victim. Atomic so
/// the read in `acks` cannot be elided or torn against the increment in the
/// handler.
static ACKS: AtomicU32 = AtomicU32::new(0);

fn acks() -> u32 {
    ACKS.load(Ordering::SeqCst)
}

extern "C" fn usr1_handler(_signo: Signal) {
    puts("A: handler ran\n");
}

/// Runs in the driver when the victim answers a ping.
extern "C" fn on_ack(_signo: Signal) {
    ACKS.fetch_add(1, Ordering::SeqCst);
}

/// Runs in the victim when the driver pings it. The reply is what proves the
/// victim was scheduled and able to take a signal.
extern "C" fn on_ping(_signo: Signal) {
    kill(1, ACK);
}

extern "C" fn segv_handler(_signo: Signal) {
    puts("A: segv handled\n");
    exit(0);
}

/// Roughly 4 seconds. The victim has to be created, scheduled, and have its
/// ELF loaded before it can answer, which alone took about 250ms in a run that
/// exposed this race, so readiness gets generous slack.
const READY_BUDGET: u32 = 300;

/// Roughly 850ms, enough for one ping to the victim and its reply back.
const ACK_BUDGET: u32 = 60;

/// Roughly 280ms to let the timer tick actually park or reap B before probing.
const SETTLE_BUDGET: u32 = 20;

/// Roughly 560ms of pumping used to prove an ack does NOT arrive.
const NEG_BUDGET: u32 = 40;

/// Reported deltas are clamped so a runaway counter shows a bounded, obviously
/// wrong number instead of a confusing giant value.
const REPORT_CLAMP: u32 = 99;

fn role_a() {
    puts("A: start\n");

    install_handler(Signal::Usr1, usr1_handler);

    let usr1_set: SigSet = Signal::Usr1.bit();
    sigprocmask(SigMaskHow::Block, Some(&usr1_set), None);
    kill(0, Signal::Usr1);

    let mut pending: SigSet = 0;
    sigpending(&mut pending);
    if pending & Signal::Usr1.bit() != 0 {
        puts("A: pending ok\n");
    }

    sigprocmask(SigMaskHow::Unblock, Some(&usr1_set), None);
    puts("A: after unblock\n");

    let ready = if wait_acks(1, READY_BUDGET) { 1 } else { 0 };

    // A ping answered while B runs normally is the baseline the later phases
    // are compared against.
    let before = acks();
    kill(2, PING);
    wait_acks(before + 1, ACK_BUDGET);
    let pre = acks().saturating_sub(before).min(REPORT_CLAMP);

    // Stop B, let the tick park it, then ping. A stopped process cannot reach a
    // syscall exit, so the ping stays pending and no ack must arrive.
    kill(2, Signal::Stop);
    spin(SETTLE_BUDGET);
    let before = acks();
    kill(2, PING);
    spin(NEG_BUDGET);
    let stop = acks().saturating_sub(before).min(REPORT_CLAMP);

    // Continue B. The ping left pending across the stop is delivered on resume,
    // so exactly one ack must arrive.
    let before = acks();
    kill(2, Signal::Continue);
    wait_acks(before + 1, ACK_BUDGET);
    let cont = acks().saturating_sub(before).min(REPORT_CLAMP);

    // Terminate B, then ping the dead process. No ack must arrive.
    kill(2, Signal::Terminate);
    spin(SETTLE_BUDGET);
    let before = acks();
    kill(2, PING);
    spin(NEG_BUDGET);
    let term = acks().saturating_sub(before).min(REPORT_CLAMP);

    print_report(ready, pre, stop, cont, term);

    // Regression guard: a syscall that copies out to a bad user pointer must
    // fail with EFAULT, not fault inside the kernel and panic it. Usr2 keeps
    // this disjoint from the handshake; new=0 leaves its disposition untouched
    // while old points at an unmapped lower-half address.
    const SYS_SIGACTION: usize = 43;
    const EFAULT: isize = 20;
    let efault = syscall3(SYS_SIGACTION, Signal::Usr2.number() as usize, 0, 0x43) as isize;
    if efault == -EFAULT {
        puts("A: efault ok\n");
    }

    install_handler(Signal::Segfault, segv_handler);
    let null = core::ptr::null::<u8>();
    let _ = unsafe { core::ptr::read_volatile(null) };

    puts("A: UNREACHABLE\n");
}

fn role_b() -> ! {
    // The ping handler is already armed by `_start`. pid 1 is created before
    // pid 2, so the driver always exists to receive this announcement.
    kill(1, ACK);

    // Keep making syscalls so the ping handler can run at each syscall exit.
    loop {
        pump();
    }
}

/// Write the decimal digits of `value` into `buf` starting at `at`, returning
/// the index just past the last digit written.
fn write_u32(buf: &mut [u8], at: usize, value: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut count = 0;
    let mut v = value;
    loop {
        digits[count] = b'0' + (v % 10) as u8;
        count += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let mut len = at;
    while count > 0 {
        count -= 1;
        buf[len] = digits[count];
        len += 1;
    }
    len
}

/// Copy `bytes` into `buf` starting at `at`, returning the new end index.
fn write_bytes(buf: &mut [u8], at: usize, bytes: &[u8]) -> usize {
    let mut len = at;
    for &b in bytes {
        buf[len] = b;
        len += 1;
    }
    len
}

fn print_report(ready: u32, pre: u32, stop: u32, cont: u32, term: u32) {
    let mut buf = [0u8; 64];
    let mut len = write_bytes(&mut buf, 0, b"A: report ready=");
    len = write_u32(&mut buf, len, ready);
    len = write_bytes(&mut buf, len, b" pre=");
    len = write_u32(&mut buf, len, pre);
    len = write_bytes(&mut buf, len, b" stop=");
    len = write_u32(&mut buf, len, stop);
    len = write_bytes(&mut buf, len, b" cont=");
    len = write_u32(&mut buf, len, cont);
    len = write_bytes(&mut buf, len, b" term=");
    len = write_u32(&mut buf, len, term);
    buf[len] = b'\n';
    len += 1;
    write(1, &buf[..len]);
}

fn print_pid(pid: i64) {
    let mut buf = [0u8; 24];
    let mut len = write_bytes(&mut buf, 0, b"init: pid=");
    if pid < 0 {
        buf[len] = b'-';
        len += 1;
    }
    len = write_u32(&mut buf, len, pid.unsigned_abs() as u32);
    buf[len] = b'\n';
    len += 1;
    write(1, &buf[..len]);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    // Both handshake handlers are armed before any other syscall, and before
    // the role is even known, because the peer may already be running. The
    // kernel installs the new action during the syscall and only then drains
    // pending signals, so a peer signal that arrived first is delivered against
    // the handler this very call installs instead of hitting the default
    // action. Ordering the ack first matters, it is the only one of the two
    // that can already be pending here.
    install_handler(ACK, on_ack);
    install_handler(PING, on_ping);

    let pid = getpid();
    print_pid(pid);

    match pid {
        1 => role_a(),
        2 => role_b(),
        _ => {
            puts("init: unexpected pid\n");
        }
    }

    exit(0);
}
