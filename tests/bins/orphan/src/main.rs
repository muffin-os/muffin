#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use minilib::{Signal, fork, getppid, install_handler, kill, nanosleep, println, waitpid};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_POLLS: usize = 500;
// Spawn order fixes the pids: test-init is 1, this program 2, its child 3, the grandchild 4.
const PARENT_PID: i64 = 2;
const INIT_PID: i64 = 1;

static DONE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_done(_signo: Signal) {
    DONE.store(true, Ordering::SeqCst);
}

minilib::entry!(main);

fn grandchild() -> i32 {
    let mut ppid = getppid();
    for _ in 0..MAX_POLLS {
        if ppid == INIT_PID {
            break;
        }
        let _ = nanosleep(&POLL_INTERVAL.into(), None);
        ppid = getppid();
    }
    println!("orphan: grandchild ppid={ppid}");
    if let Err(e) = kill(PARENT_PID, Signal::Usr1) {
        println!("orphan: FAIL kill {e}");
        return 1;
    }
    0
}

fn middle() -> i32 {
    match fork() {
        Ok(0) => grandchild(),
        Ok(_) => {
            println!("orphan: middle exiting");
            0
        }
        Err(e) => {
            println!("orphan: FAIL fork {e}");
            1
        }
    }
}

fn parent() -> i32 {
    let mut status = 0;
    match waitpid(-1, &mut status) {
        Ok(pid) => println!("orphan: reaped pid={pid} status={status}"),
        Err(e) => {
            println!("orphan: FAIL waitpid {e}");
            return 1;
        }
    }
    match waitpid(-1, &mut status) {
        Ok(pid) => {
            println!("orphan: FAIL second wait returned {pid}");
            return 1;
        }
        Err(e) => println!("orphan: second wait {e}"),
    }
    for _ in 0..MAX_POLLS {
        if DONE.load(Ordering::SeqCst) {
            println!("orphan: grandchild done");
            return 0;
        }
        let _ = nanosleep(&POLL_INTERVAL.into(), None);
    }
    println!("orphan: FAIL grandchild timeout");
    1
}

fn main() -> i32 {
    if let Err(e) = install_handler(Signal::Usr1, on_done) {
        println!("orphan: FAIL install_handler {e}");
        return 1;
    }
    match fork() {
        Ok(0) => middle(),
        Ok(_) => parent(),
        Err(e) => {
            println!("orphan: FAIL fork {e}");
            1
        }
    }
}
