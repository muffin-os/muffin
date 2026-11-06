//! This program exercises threads in the Rust standard library on muffin.
//!
//! Each step prints `threads: <step> ok`. The first failure prints
//! `threads: FAIL <step>: <error>` and exits 1 at once.

use std::cell::Cell;
use std::error::Error;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};
use std::{ptr, thread};

use kernel_abi::{SYS_THREAD_SELF, Signal};
use libc::{SIG_BLOCK, SIG_DFL, SIG_SETMASK, SIGUSR1, pthread_t, sighandler_t, sigset_t};

type Step = fn() -> Result<(), Box<dyn Error>>;

// `sigmask` runs before `detach`. The detached thread can still run when the
// next step starts, and the kernel can pick it for the process signal.
const STEPS: &[(&str, Step)] = &[
    ("spawn join", spawn_join),
    ("self id", self_id),
    ("mutex counter", mutex_counter),
    ("channel", channel),
    ("condvar", condvar),
    ("thread local", thread_locals),
    ("sigmask", sigmask),
    ("detach", detach),
];

/// This is the longest wait for another thread. A correct run needs only a small part of it.
const WAIT_LIMIT: Duration = Duration::from_secs(10);

fn main() {
    for (name, step) in STEPS {
        if let Err(e) = step() {
            println!("threads: FAIL {name}: {e}");
            std::process::exit(1);
        }
        println!("threads: {name} ok");
    }
    println!("threads: done");
}

fn check(ok: bool, what: &str) -> Result<(), Box<dyn Error>> {
    if ok { Ok(()) } else { Err(what.into()) }
}

fn poisoned<T>(_: PoisonError<T>) -> Box<dyn Error> {
    "a mutex is poisoned".into()
}

fn panicked<T>(_: T) -> Box<dyn Error> {
    "a spawned thread panicked".into()
}

/// This function polls `done` until it returns true or until `WAIT_LIMIT` passes.
fn wait_for(done: impl Fn() -> bool, what: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + WAIT_LIMIT;
    while !done() {
        check(Instant::now() < deadline, what)?;
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn spawn_join() -> Result<(), Box<dyn Error>> {
    let answer = thread::spawn(|| 6 * 7).join().map_err(panicked)?;
    check(answer == 42, "join returned another value")
}

/// This holds the id of the calling thread, as the runtime and the kernel report it.
#[derive(Clone, Copy, Debug)]
struct ThreadIds {
    pthread_self: pthread_t,
    kernel: usize,
}

impl ThreadIds {
    fn of_caller() -> Self {
        Self {
            pthread_self: unsafe { libc::pthread_self() },
            kernel: unsafe { muffin_sys::syscall0(SYS_THREAD_SELF) },
        }
    }
}

fn self_id() -> Result<(), Box<dyn Error>> {
    let main_ids = ThreadIds::of_caller();
    let spawned = thread::spawn(ThreadIds::of_caller)
        .join()
        .map_err(panicked)?;
    check(
        main_ids.pthread_self == main_ids.kernel,
        &format!("main thread {main_ids:?}"),
    )?;
    check(
        spawned.pthread_self == spawned.kernel,
        &format!("spawned thread {spawned:?}"),
    )?;
    check(
        main_ids.pthread_self != spawned.pthread_self,
        "both threads have the same pthread_self",
    )
}

fn mutex_counter() -> Result<(), Box<dyn Error>> {
    const THREADS: u64 = 8;
    const ADDS: u64 = 1000;
    let counter = Arc::new(Mutex::new(0u64));
    let adders: Vec<_> = (0..THREADS)
        .map(|_| {
            let counter = Arc::clone(&counter);
            thread::spawn(move || {
                for _ in 0..ADDS {
                    *counter.lock().expect("counter mutex is poisoned") += 1;
                }
            })
        })
        .collect();
    for adder in adders {
        adder.join().map_err(panicked)?;
    }
    let total = *counter.lock().map_err(poisoned)?;
    check(total == THREADS * ADDS, &format!("the total is {total}"))
}

fn channel() -> Result<(), Box<dyn Error>> {
    const PINGS: u32 = 100;
    let (to_echo, echo_inbox) = mpsc::channel::<u32>();
    let (to_main, main_inbox) = mpsc::channel::<u32>();
    let echo = thread::spawn(move || {
        for ping in echo_inbox {
            if to_main.send(ping + 1).is_err() {
                return;
            }
        }
    });
    for ping in 0..PINGS {
        to_echo.send(ping)?;
        let pong = main_inbox.recv_timeout(WAIT_LIMIT)?;
        check(pong == ping + 1, &format!("ping {ping} got pong {pong}"))?;
    }
    drop(to_echo);
    echo.join().map_err(panicked)
}

#[derive(Default)]
struct Gate {
    is_waiting: bool,
    is_open: bool,
}

fn condvar() -> Result<(), Box<dyn Error>> {
    let gate = Arc::new((Mutex::new(Gate::default()), Condvar::new()));
    let waiter = {
        let gate = Arc::clone(&gate);
        thread::spawn(move || {
            let (state, changed) = &*gate;
            let mut state = state.lock().expect("gate mutex is poisoned");
            state.is_waiting = true;
            changed.notify_all();
            let (_state, waited) = changed
                .wait_timeout_while(state, WAIT_LIMIT, |state| !state.is_open)
                .expect("gate mutex is poisoned");
            !waited.timed_out()
        })
    };
    {
        let (state, changed) = &*gate;
        let state = state.lock().map_err(poisoned)?;
        // The waiter sets `is_waiting` and waits under the same mutex hold, so
        // the notify below finds the waiter in the wait.
        let (mut state, waited) = changed
            .wait_timeout_while(state, WAIT_LIMIT, |state| !state.is_waiting)
            .map_err(poisoned)?;
        check(!waited.timed_out(), "the waiter did not start to wait")?;
        state.is_open = true;
        changed.notify_all();
    }
    check(
        waiter.join().map_err(panicked)?,
        "the waiter missed the notify",
    )?;

    const TIMEOUT: Duration = Duration::from_millis(50);
    let (lonely, never) = (Mutex::new(()), Condvar::new());
    let start = Instant::now();
    let (_guard, waited) = never
        .wait_timeout(lonely.lock().map_err(poisoned)?, TIMEOUT)
        .map_err(poisoned)?;
    check(
        waited.timed_out(),
        "wait_timeout without a notify did not time out",
    )?;
    check(
        start.elapsed() >= TIMEOUT,
        "wait_timeout returned before its timeout",
    )
}

thread_local! {
    static SLOT: Cell<u32> = const { Cell::new(0) };
}

/// This holds the value that the destructor of the test key receives. It is 0 before the destructor runs.
static DESTROYED_VALUE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn record_destroyed_value(value: *mut c_void) {
    DESTROYED_VALUE.store(value as usize, Ordering::SeqCst);
}

fn thread_locals() -> Result<(), Box<dyn Error>> {
    SLOT.set(1);
    let seen = thread::spawn(|| {
        let initial = SLOT.get();
        SLOT.set(2);
        initial
    })
    .join()
    .map_err(panicked)?;
    check(
        seen == 0,
        &format!("the spawned thread started with {seen}"),
    )?;
    check(
        SLOT.get() == 1,
        "the spawned thread changed the main thread's value",
    )?;

    const KEY_VALUE: usize = 0x5eed;
    let mut key = 0;
    let created = unsafe { libc::pthread_key_create(&mut key, Some(record_destroyed_value)) };
    check(
        created == 0,
        &format!("pthread_key_create returned {created}"),
    )?;
    let stored = thread::spawn(move || unsafe {
        libc::pthread_setspecific(key, ptr::without_provenance(KEY_VALUE))
    })
    .join()
    .map_err(panicked)?;
    check(
        stored == 0,
        &format!("pthread_setspecific returned {stored}"),
    )?;
    let destroyed = DESTROYED_VALUE.load(Ordering::SeqCst);
    check(
        destroyed == KEY_VALUE,
        &format!("the key destructor got {destroyed:#x} at the join"),
    )?;
    let deleted = unsafe { libc::pthread_key_delete(key) };
    check(
        deleted == 0,
        &format!("pthread_key_delete returned {deleted}"),
    )
}

/// This holds the `pthread_self` of the thread that runs the `SIGUSR1` handler. It is 0 before the handler runs.
static HANDLED_BY: AtomicUsize = AtomicUsize::new(0);

extern "C" fn record_handler_thread(_: c_int) {
    HANDLED_BY.store(unsafe { libc::pthread_self() }, Ordering::SeqCst);
}

fn sigmask() -> Result<(), Box<dyn Error>> {
    let handler = record_handler_thread as extern "C" fn(c_int) as sighandler_t;
    check(
        unsafe { libc::signal(SIGUSR1, handler) } != libc::SIG_ERR,
        "signal failed",
    )?;

    // The receiver starts before the main thread blocks the signal, so the
    // receiver inherits an empty mask.
    let (to_main, receiver_tid) = mpsc::channel();
    let (to_receiver, finish) = mpsc::channel::<()>();
    let receiver = thread::spawn(move || {
        let _ = to_main.send(unsafe { libc::pthread_self() });
        let _ = finish.recv();
    });
    let receiver_tid = receiver_tid.recv_timeout(WAIT_LIMIT)?;

    let usr1: sigset_t = Signal::Usr1.bit().bits();
    let mut old: sigset_t = 0;
    let blocked = unsafe { libc::pthread_sigmask(SIG_BLOCK, &usr1, &mut old) };
    check(blocked == 0, &format!("pthread_sigmask returned {blocked}"))?;
    let killed = unsafe { libc::kill(libc::getpid(), SIGUSR1) };
    check(killed == 0, "kill failed")?;
    let handled = wait_for(
        || HANDLED_BY.load(Ordering::SeqCst) != 0,
        "no thread ran the SIGUSR1 handler",
    );

    let _ = to_receiver.send(());
    receiver.join().map_err(panicked)?;
    unsafe { libc::pthread_sigmask(SIG_SETMASK, &old, ptr::null_mut()) };
    unsafe { libc::signal(SIGUSR1, SIG_DFL) };

    handled?;
    let handled_by = HANDLED_BY.load(Ordering::SeqCst);
    check(
        handled_by == receiver_tid,
        &format!("the handler ran in thread {handled_by}, not in the receiver {receiver_tid}"),
    )
}

fn detach() -> Result<(), Box<dyn Error>> {
    let finished = Arc::new(AtomicBool::new(false));
    let detached = {
        let finished = Arc::clone(&finished);
        thread::spawn(move || finished.store(true, Ordering::Release))
    };
    drop(detached);
    wait_for(
        || finished.load(Ordering::Acquire),
        "the detached thread did not finish",
    )
}
