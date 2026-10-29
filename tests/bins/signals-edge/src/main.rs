#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use minilib::{
    SYS_KILL, SYS_SIGRETURN, SaFlags, SigMaskHow, Signal, getpid, handler_action, install_handler,
    kill, println, sigaction, sigprocmask, syscall0,
};

// Same spin as `PUMP_SPIN` in signals-init, which documents it.
const PUMP_SPIN: u64 = 2_000_000;
const DELIVER_BUDGET: u32 = 60;
const NEST_BUDGET: u32 = 120;
const NEG_BUDGET: u32 = 40;

static URGENT_RUNS: AtomicU32 = AtomicU32::new(0);
static WINCH_RUNS: AtomicU32 = AtomicU32::new(0);
static CHILD_RUNS: AtomicU32 = AtomicU32::new(0);

/// Spawn order fixes the pid, so the pid picks the role. pid 1 is test-init.
#[derive(Clone, Copy)]
enum Role {
    A = 2,
    B = 3,
    C = 4,
    D = 5,
    E = 6,
    F = 7,
}

impl Role {
    fn of(pid: i64) -> Option<Self> {
        [Self::A, Self::B, Self::C, Self::D, Self::E, Self::F]
            .into_iter()
            .find(|role| role.pid() == pid)
    }

    fn pid(self) -> i64 {
        self as i64
    }
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

extern "C" fn urgent_handler(_signo: Signal) {
    URGENT_RUNS.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn winch_handler(_signo: Signal) {
    let run = WINCH_RUNS.fetch_add(1, Ordering::SeqCst) + 1;
    if run == 1 {
        let _ = kill(Role::A.pid(), Signal::WindowChanged);
        spin(NEG_BUDGET);
        if WINCH_RUNS.load(Ordering::SeqCst) == 1 {
            println!("A: defer ok");
        }
    }
}

extern "C" fn child_handler(_signo: Signal) {
    let run = CHILD_RUNS.fetch_add(1, Ordering::SeqCst) + 1;
    if run == 1 {
        let _ = kill(Role::A.pid(), Signal::Child);
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

    let urgent = handler_action(urgent_handler, SaFlags::RESETHAND);
    let _ = sigaction(Signal::Urgent, Some(&urgent), None);

    let _ = kill(Role::A.pid(), Signal::Urgent);
    if wait_runs(&URGENT_RUNS, 1, DELIVER_BUDGET) {
        let _ = kill(Role::A.pid(), Signal::Urgent);
        spin(NEG_BUDGET);
        if URGENT_RUNS.load(Ordering::SeqCst) == 1 {
            println!("A: resethand ok");
        }
    }

    let _ = install_handler(Signal::WindowChanged, winch_handler);
    let _ = kill(Role::A.pid(), Signal::WindowChanged);
    if wait_runs(&WINCH_RUNS, 2, NEST_BUDGET) {
        println!("A: redeliver ok");
    }

    let child = handler_action(child_handler, SaFlags::NODEFER);
    let _ = sigaction(Signal::Child, Some(&child), None);
    let _ = kill(Role::A.pid(), Signal::Child);
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

    if let Err(e) = install_handler(Signal::Segfault, b_segv_handler) {
        println!("B: FAIL install_handler {e}");
        return;
    }
    let segv = Signal::Segfault.bit();
    if let Err(e) = sigprocmask(SigMaskHow::Block, Some(&segv), None) {
        println!("B: FAIL sigprocmask {e}");
        return;
    }

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
    match Role::of(getpid()) {
        Some(Role::A) => role_a(),
        Some(Role::B) => role_b(),
        Some(Role::C) => role_c(),
        Some(Role::D) => role_d(),
        Some(Role::E) => role_e(),
        Some(Role::F) => role_f(),
        None => println!("edge: unexpected pid"),
    }

    0
}
