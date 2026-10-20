#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use minilib::{
    EFAULT, EINVAL, ESRCH, SYS_KILL, SYS_SIGACTION, SYS_SIGPENDING, SYS_SIGPROCMASK, SYS_SIGRETURN,
    SaFlags, SigAction, SigHandler, SigMaskHow, SigSet, Signal, getpid, install_handler, kill,
    println, sigaction, sigprocmask, sigreturn_restorer, syscall0, syscall1, syscall2, syscall3,
};

// Same spin as `PUMP_SPIN` in signals-init, which documents it.
const PUMP_SPIN: u64 = 2_000_000;
const DELIVER_BUDGET: u32 = 60;
const NEST_BUDGET: u32 = 120;
const NEG_BUDGET: u32 = 40;

static URGENT_RUNS: AtomicU32 = AtomicU32::new(0);
static WINCH_RUNS: AtomicU32 = AtomicU32::new(0);
static CHILD_RUNS: AtomicU32 = AtomicU32::new(0);

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
        mask: SigSet::EMPTY,
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
            println!("A: defer ok");
        }
    }
}

extern "C" fn child_handler(_signo: Signal) {
    let run = CHILD_RUNS.fetch_add(1, Ordering::SeqCst) + 1;
    if run == 1 {
        let _ = kill(1, Signal::Child);
        if wait_runs(&CHILD_RUNS, 2, NEST_BUDGET) {
            println!("A: nodefer nested ok");
        }
    }
}

extern "C" fn noop_handler(_signo: Signal) {}

extern "C" fn b_segv_handler(_signo: Signal) {
    println!("B: UNREACHABLE");
}

fn role_a() {
    println!("A: start");

    if unsafe { syscall2(SYS_KILL, 1, 0) } == 0 {
        println!("A: sig0 self ok");
    }

    if unsafe { syscall2(SYS_KILL, 999, 0) } as isize == -isize::from(ESRCH) {
        println!("A: sig0 esrch ok");
    }

    if unsafe { syscall2(SYS_KILL, 1, Signal::COUNT + 1) } as isize == -isize::from(EINVAL) {
        println!("A: kill einval ok");
    }

    if unsafe { syscall3(SYS_SIGPROCMASK, 99, 0, 0) } as isize == -isize::from(EINVAL) {
        println!("A: how einval ok");
    }

    let protected = custom_action(noop_handler, SaFlags::default());
    let kill_rc = sigaction(Signal::Kill, Some(&protected), None);
    let stop_rc = sigaction(Signal::Stop, Some(&protected), None);
    if kill_rc == Err(EINVAL) && stop_rc == Err(EINVAL) {
        println!("A: protected einval ok");
    }

    let no_restorer = SigAction {
        handler: SigHandler::new(noop_handler as *const () as usize),
        mask: SigSet::EMPTY,
        flags: SaFlags::default(),
        restorer: 0,
    };
    if sigaction(Signal::Usr1, Some(&no_restorer), None) == Err(EINVAL) {
        println!("A: restorer einval ok");
    }

    // Safety: both pointers are deliberately invalid, and the kernel rejecting
    // them before any access is what the check asserts.
    let unmapped =
        unsafe { syscall3(SYS_SIGACTION, Signal::Usr2.number() as usize, 0x43, 0) } as isize;
    let upper =
        unsafe { syscall3(SYS_SIGACTION, Signal::Usr2.number() as usize, 1 << 63, 0) } as isize;
    if unmapped == -isize::from(EFAULT) && upper == -isize::from(EFAULT) {
        println!("A: efault new ok");
    }

    if unsafe { syscall1(SYS_SIGPENDING, 0) } as isize == -isize::from(EINVAL) {
        println!("A: sigpending einval ok");
    }

    let _ = sigprocmask(SigMaskHow::Block, Some(&SigSet::ALL), None);
    let mut current = SigSet::EMPTY;
    let _ = sigprocmask(SigMaskHow::Block, None, Some(&mut current));
    let unblockable = Signal::Kill.bit().union(Signal::Stop.bit());
    if current.intersection(unblockable).is_empty() && current.contains(Signal::Terminate) {
        println!("A: unblockable ok");
    }
    let _ = sigprocmask(SigMaskHow::SetMask, Some(&SigSet::EMPTY), None);

    let urgent = custom_action(urgent_handler, SaFlags::RESETHAND);
    let _ = sigaction(Signal::Urgent, Some(&urgent), None);
    let mut installed = SigAction::default();
    let _ = sigaction(Signal::Urgent, None, Some(&mut installed));
    if installed.handler.addr() == urgent_handler as *const () as usize {
        println!("A: query ok");
    }

    let _ = kill(1, Signal::Urgent);
    if wait_runs(&URGENT_RUNS, 1, DELIVER_BUDGET) {
        let _ = kill(1, Signal::Urgent);
        spin(NEG_BUDGET);
        if URGENT_RUNS.load(Ordering::SeqCst) == 1 {
            println!("A: resethand ok");
        }
    }

    let _ = install_handler(Signal::WindowChanged, winch_handler);
    let _ = kill(1, Signal::WindowChanged);
    if wait_runs(&WINCH_RUNS, 2, NEST_BUDGET) {
        println!("A: redeliver ok");
    }

    let child = custom_action(child_handler, SaFlags::NODEFER);
    let _ = sigaction(Signal::Child, Some(&child), None);
    let _ = kill(1, Signal::Child);
    wait_runs(&CHILD_RUNS, 2, NEST_BUDGET);

    forged_sigreturn();

    println!("A: UNREACHABLE");
}

#[inline(never)]
fn forged_sigreturn() {
    let mut pad = [0u8; 4096];
    let mut i = 0;
    while i < pad.len() {
        unsafe { core::ptr::write_volatile(&mut pad[i], 0) };
        i += 64;
    }
    println!("A: forged sigreturn next");
    unsafe { syscall0(SYS_SIGRETURN) };
}

fn role_b() {
    println!("B: start");

    let _ = install_handler(Signal::Segfault, b_segv_handler);
    let segv = Signal::Segfault.bit();
    let _ = sigprocmask(SigMaskHow::Block, Some(&segv), None);

    println!("B: blocked fault next");
    let null = core::ptr::null::<u8>();
    let _ = unsafe { core::ptr::read_volatile(null) };

    println!("B: UNREACHABLE");
}

fn role_c() {
    println!("C: forged sigreturn next");
    unsafe { syscall0(SYS_SIGRETURN) };

    println!("C: UNREACHABLE");
}

// Raise `Usr1` with `rsp` pointing into the unmapped page below the ELF
// image. The handler is installed, so the kernel must build a frame on this
// stack and has nowhere to put it.
fn role_d() {
    let _ = install_handler(Signal::Usr1, noop_handler);

    println!("D: wild stack delivery next");
    let pid = getpid() as usize;
    let wild_rsp: usize = 0x1000;
    unsafe {
        // Safety: rsp is swapped back right after the trap. The kernel is
        // expected to terminate the process before that instruction runs.
        core::arch::asm!(
            "xchg rsp, {wild}",
            "int 0x80",
            "xchg rsp, {wild}",
            wild = inout(reg) wild_rsp => _,
            inout("rax") SYS_KILL => _,
            in("rdi") pid,
            in("rsi") Signal::Usr1.number() as usize,
        );
    }

    println!("D: UNREACHABLE");
}

fn role_e() {
    println!("E: ud2 next");
    unsafe { core::arch::asm!("ud2") };

    println!("E: UNREACHABLE");
}

fn role_f() {
    println!("F: hlt next");
    unsafe { core::arch::asm!("hlt") };

    println!("F: UNREACHABLE");
}

minilib::entry!(main);

fn main() -> i32 {
    match getpid() {
        1 => role_a(),
        2 => role_b(),
        3 => role_c(),
        4 => role_d(),
        5 => role_e(),
        6 => role_f(),
        _ => println!("edge: unexpected pid"),
    }

    0
}
