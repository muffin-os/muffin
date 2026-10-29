#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use minilib::{Signal, execve, fork, getpid, install_handler, kill, nanosleep, println};

static DONE: AtomicBool = AtomicBool::new(false);
static mut COUNTER: u64 = 1;

extern "C" fn on_usr1(_signal: Signal) {
    DONE.store(true, Ordering::SeqCst);
}

minilib::entry!(main);

fn main() -> i32 {
    if install_handler(Signal::Usr1, on_usr1).is_err() {
        println!("fork: FAIL handler");
        return 1;
    }
    let parent = getpid();
    let mut local = 5u64;

    match fork() {
        Ok(0) => {
            println!("fork: child pid={} parent={parent}", getpid());
            local += 1;
            // Safety: the child has a private copy and only this thread runs.
            let counter = unsafe {
                COUNTER += 1;
                COUNTER
            };
            println!("fork: child local={local} counter={counter}");
            if kill(parent, Signal::Usr1).is_err() {
                println!("fork: FAIL kill");
                return 1;
            }
            let errno = execve("/bin/exit-code", &["exit-code"], &[]);
            println!("fork: FAIL exec {errno}");
            1
        }
        Ok(pid) => {
            while !DONE.load(Ordering::SeqCst) {
                let _ = nanosleep(&Duration::from_millis(10).into(), None);
            }
            // The serial device interleaves concurrent writers, so the parent
            // prints only after the exec'd child has printed and exited.
            let _ = nanosleep(&Duration::from_millis(500).into(), None);
            println!("fork: parent child={pid}");
            // Safety: the child's write landed in its own copy.
            let counter = unsafe { COUNTER };
            println!("fork: parent local={local} counter={counter}");
            42
        }
        Err(e) => {
            println!("fork: FAIL {e}");
            1
        }
    }
}
