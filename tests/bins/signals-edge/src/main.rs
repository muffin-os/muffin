#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use minilib::{
    EFAULT, EINVAL, ESRCH, SYS_KILL, SYS_SIGACTION, SYS_SIGPENDING, SYS_SIGPROCMASK, SYS_SIGRETURN,
    SaFlags, SigAction, SigHandler, SigMaskHow, SigSet, Signal, exit, getpid, install_handler,
    kill, sigaction, sigprocmask, sigreturn_restorer, syscall0, syscall1, syscall2, syscall3,
    write,
};

const PUMP_SPIN: u64 = 2_000_000;
const DELIVER_BUDGET: u32 = 60;
const NEST_BUDGET: u32 = 120;
const NEG_BUDGET: u32 = 40;

static URGENT_RUNS: AtomicU32 = AtomicU32::new(0);
static WINCH_RUNS: AtomicU32 = AtomicU32::new(0);
static CHILD_RUNS: AtomicU32 = AtomicU32::new(0);

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

fn busy_delay_n(n: u64) {
    let mut counter: u64 = 0;
    while counter < n {
        counter = unsafe { core::ptr::read_volatile(&counter) } + 1;
    }
}

fn pump() {
    busy_delay_n(PUMP_SPIN);
}

fn spin(budget: u32) {
    let mut i = 0;
    while i < budget {
        pump();
        i += 1;
    }
}

fn wait_runs(counter: &AtomicU32, target: u32, budget: u32) -> bool {
    if counter.load(Ordering::SeqCst) >= target {
        return true;
    }
    let mut i = 0;
    while i < budget {
        pump();
        if counter.load(Ordering::SeqCst) >= target {
            return true;
        }
        i += 1;
    }
    counter.load(Ordering::SeqCst) >= target
}

fn custom_action(handler: extern "C" fn(Signal), flags: SaFlags) -> SigAction {
    SigAction {
        handler: SigHandler::new(handler as usize),
        mask: 0,
        flags,
        restorer: sigreturn_restorer as *const () as usize,
    }
}

extern "C" fn urgent_handler(_signo: Signal) {
    URGENT_RUNS.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn winch_handler(_signo: Signal) {
    let run = WINCH_RUNS.fetch_add(1, Ordering::SeqCst) + 1;
    if run == 1 {
        let _ = kill(1, Signal::WindowChanged);
        spin(NEG_BUDGET);
        if WINCH_RUNS.load(Ordering::SeqCst) == 1 {
            puts("A: defer ok\n");
        }
    }
}

extern "C" fn child_handler(_signo: Signal) {
    let run = CHILD_RUNS.fetch_add(1, Ordering::SeqCst) + 1;
    if run == 1 {
        let _ = kill(1, Signal::Child);
        if wait_runs(&CHILD_RUNS, 2, NEST_BUDGET) {
            puts("A: nodefer nested ok\n");
        }
    }
}

extern "C" fn noop_handler(_signo: Signal) {}

extern "C" fn b_segv_handler(_signo: Signal) {
    puts("B: UNREACHABLE\n");
}

fn role_a() {
    puts("A: start\n");

    if syscall2(SYS_KILL, 1, 0) == 0 {
        puts("A: sig0 self ok\n");
    }

    if syscall2(SYS_KILL, 999, 0) as isize == -isize::from(ESRCH) {
        puts("A: sig0 esrch ok\n");
    }

    if syscall2(SYS_KILL, 1, Signal::COUNT + 1) as isize == -isize::from(EINVAL) {
        puts("A: kill einval ok\n");
    }

    if syscall3(SYS_SIGPROCMASK, 99, 0, 0) as isize == -isize::from(EINVAL) {
        puts("A: how einval ok\n");
    }

    let protected = custom_action(noop_handler, SaFlags::default());
    let kill_rc = sigaction(Signal::Kill, Some(&protected), None);
    let stop_rc = sigaction(Signal::Stop, Some(&protected), None);
    if kill_rc == Err(EINVAL) && stop_rc == Err(EINVAL) {
        puts("A: protected einval ok\n");
    }

    let no_restorer = SigAction {
        handler: SigHandler::new(noop_handler as *const () as usize),
        mask: 0,
        flags: SaFlags::default(),
        restorer: 0,
    };
    if sigaction(Signal::Usr1, Some(&no_restorer), None) == Err(EINVAL) {
        puts("A: restorer einval ok\n");
    }

    let unmapped = syscall3(SYS_SIGACTION, Signal::Usr2.number() as usize, 0x43, 0) as isize;
    let upper = syscall3(SYS_SIGACTION, Signal::Usr2.number() as usize, 1 << 63, 0) as isize;
    if unmapped == -isize::from(EFAULT) && upper == -isize::from(EFAULT) {
        puts("A: efault new ok\n");
    }

    if syscall1(SYS_SIGPENDING, 0) as isize == -isize::from(EINVAL) {
        puts("A: sigpending einval ok\n");
    }

    let full: SigSet = u64::MAX;
    let _ = sigprocmask(SigMaskHow::Block, Some(&full), None);
    let mut current: SigSet = 0;
    let _ = sigprocmask(SigMaskHow::Block, None, Some(&mut current));
    let unblockable = Signal::Kill.bit() | Signal::Stop.bit();
    if current & unblockable == 0 && current & Signal::Terminate.bit() != 0 {
        puts("A: unblockable ok\n");
    }
    let empty: SigSet = 0;
    let _ = sigprocmask(SigMaskHow::SetMask, Some(&empty), None);

    let urgent = custom_action(urgent_handler, SaFlags::RESETHAND);
    let _ = sigaction(Signal::Urgent, Some(&urgent), None);
    let mut installed = SigAction::default();
    let _ = sigaction(Signal::Urgent, None, Some(&mut installed));
    if installed.handler.addr() == urgent_handler as *const () as usize {
        puts("A: query ok\n");
    }

    let _ = kill(1, Signal::Urgent);
    if wait_runs(&URGENT_RUNS, 1, DELIVER_BUDGET) {
        let _ = kill(1, Signal::Urgent);
        spin(NEG_BUDGET);
        if URGENT_RUNS.load(Ordering::SeqCst) == 1 {
            puts("A: resethand ok\n");
        }
    }

    let _ = install_handler(Signal::WindowChanged, winch_handler);
    let _ = kill(1, Signal::WindowChanged);
    if wait_runs(&WINCH_RUNS, 2, NEST_BUDGET) {
        puts("A: redeliver ok\n");
    }

    let child = custom_action(child_handler, SaFlags::NODEFER);
    let _ = sigaction(Signal::Child, Some(&child), None);
    let _ = kill(1, Signal::Child);
    wait_runs(&CHILD_RUNS, 2, NEST_BUDGET);

    forged_sigreturn();

    puts("A: UNREACHABLE\n");
}

#[inline(never)]
fn forged_sigreturn() {
    let mut pad = [0u8; 4096];
    let mut i = 0;
    while i < pad.len() {
        unsafe { core::ptr::write_volatile(&mut pad[i], 0) };
        i += 64;
    }
    puts("A: forged sigreturn next\n");
    syscall0(SYS_SIGRETURN);
}

fn role_b() {
    puts("B: start\n");

    let _ = install_handler(Signal::Segfault, b_segv_handler);
    let segv: SigSet = Signal::Segfault.bit();
    let _ = sigprocmask(SigMaskHow::Block, Some(&segv), None);

    puts("B: blocked fault next\n");
    let null = core::ptr::null::<u8>();
    let _ = unsafe { core::ptr::read_volatile(null) };

    puts("B: UNREACHABLE\n");
}

fn role_c() {
    puts("C: forged sigreturn next\n");
    syscall0(SYS_SIGRETURN);

    puts("C: UNREACHABLE\n");
}

minilib::entry!(main);

fn main() -> i32 {
    match getpid() {
        1 => role_a(),
        2 => role_b(),
        3 => role_c(),
        _ => puts("edge: unexpected pid\n"),
    }

    exit(0)
}
