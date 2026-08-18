#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use minilib::{
    EFAULT, SYS_SIGACTION, SigMaskHow, SigSet, Signal, exit, getpid, install_handler, kill,
    println, sigpending, sigprocmask, syscall3,
};

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

/// Roughly 14ms of spin per pump.
const PUMP_SPIN: u64 = 2_000_000;

/// Handlers are delivered at timer ticks that land in user mode. The spin
/// gives ticks a user frame to land on.
fn pump() {
    busy_delay_n(PUMP_SPIN);
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
    println!("A: handler ran");
}

/// Runs in the driver when the victim answers a ping.
extern "C" fn on_ack(_signo: Signal) {
    ACKS.fetch_add(1, Ordering::SeqCst);
}

/// Runs in the victim when the driver pings it. The reply is what proves the
/// victim was scheduled and able to take a signal.
extern "C" fn on_ping(_signo: Signal) {
    let _ = kill(1, ACK);
}

extern "C" fn segv_handler(_signo: Signal) {
    println!("A: segv handled");
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
    println!("A: start");

    let _ = install_handler(Signal::Usr1, usr1_handler);

    let usr1_set: SigSet = Signal::Usr1.bit();
    let _ = sigprocmask(SigMaskHow::Block, Some(&usr1_set), None);
    let _ = kill(0, Signal::Usr1);

    let mut pending: SigSet = 0;
    let _ = sigpending(&mut pending);
    if pending & Signal::Usr1.bit() != 0 {
        println!("A: pending ok");
    }

    let _ = sigprocmask(SigMaskHow::Unblock, Some(&usr1_set), None);
    println!("A: after unblock");

    let ready = if wait_acks(1, READY_BUDGET) { 1 } else { 0 };

    // A ping answered while B runs normally is the baseline the later phases
    // are compared against.
    let before = acks();
    let _ = kill(2, PING);
    wait_acks(before + 1, ACK_BUDGET);
    let pre = acks().saturating_sub(before).min(REPORT_CLAMP);

    // Stop B, let the tick park it, then ping. A stopped process is never
    // scheduled, so the ping stays pending and no ack must arrive.
    let _ = kill(2, Signal::Stop);
    spin(SETTLE_BUDGET);
    let before = acks();
    let _ = kill(2, PING);
    spin(NEG_BUDGET);
    let stop = acks().saturating_sub(before).min(REPORT_CLAMP);

    // Continue B. The ping left pending across the stop is delivered on resume,
    // so exactly one ack must arrive.
    let before = acks();
    let _ = kill(2, Signal::Continue);
    wait_acks(before + 1, ACK_BUDGET);
    let cont = acks().saturating_sub(before).min(REPORT_CLAMP);

    // Terminate B, then ping the dead process. No ack must arrive.
    let _ = kill(2, Signal::Terminate);
    spin(SETTLE_BUDGET);
    let before = acks();
    let _ = kill(2, PING);
    spin(NEG_BUDGET);
    let term = acks().saturating_sub(before).min(REPORT_CLAMP);

    println!("A: report ready={ready} pre={pre} stop={stop} cont={cont} term={term}");

    // Regression guard: a syscall that copies out to a bad user pointer must
    // fail with EFAULT, not fault inside the kernel and panic it. Usr2 keeps
    // this disjoint from the handshake. new=0 leaves its disposition untouched
    // while old points at an unmapped lower-half address.
    let efault = syscall3(SYS_SIGACTION, Signal::Usr2.number() as usize, 0, 0x43) as isize;
    if efault == -isize::from(EFAULT) {
        println!("A: efault ok");
    }

    let _ = install_handler(Signal::Segfault, segv_handler);
    let null = core::ptr::null::<u8>();
    let _ = unsafe { core::ptr::read_volatile(null) };

    println!("A: UNREACHABLE");
}

fn role_b() -> ! {
    // The ping handler is already armed by `_start`. pid 1 is created before
    // pid 2, so the driver always exists to receive this announcement.
    let _ = kill(1, ACK);

    // Spin in user mode so ticks can deliver the ping handler.
    loop {
        pump();
    }
}

minilib::entry!(main);

fn main() -> i32 {
    // Both handshake handlers are armed before any other syscall, and before
    // the role is even known, because the peer may already be running. Signals
    // are delivered at timer ticks, so a peer signal that arrived first
    // reaches the handler this call installs and never the default action.
    // Ordering the ack first matters, it is the only one of the two that can
    // already be pending here.
    let _ = install_handler(ACK, on_ack);
    let _ = install_handler(PING, on_ping);

    let pid = getpid();
    println!("init: pid={pid}");

    match pid {
        1 => role_a(),
        2 => role_b(),
        _ => {
            println!("init: unexpected pid");
        }
    }

    0
}
