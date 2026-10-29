#![no_std]
#![no_main]

use minilib::{execve, fork, getpid, getppid, println, waitpid};

static mut COUNTER: u64 = 1;

minilib::entry!(main);

fn main() -> i32 {
    let mut local = 5u64;

    match fork() {
        Ok(0) => {
            println!("fork: child pid={} ppid={}", getpid(), getppid());
            local += 1;
            // Safety: the child has a private copy and only this thread runs.
            let counter = unsafe {
                COUNTER += 1;
                COUNTER
            };
            println!("fork: child local={local} counter={counter}");
            let errno = execve("/bin/exit-code", &["exit-code"], &[]);
            println!("fork: FAIL exec {errno}");
            1
        }
        Ok(pid) => {
            let status = match waitpid(pid) {
                Ok(status) => status,
                Err(e) => {
                    println!("fork: FAIL waitpid {e}");
                    return 1;
                }
            };
            println!("fork: parent waited pid={pid} status={status}");
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
