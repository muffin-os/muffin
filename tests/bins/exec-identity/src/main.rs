#![no_std]
#![no_main]

use minilib::{args, exe_path, execve, fork, println, waitpid};

const TARGET: &str = "/bin/exec-identity-target";
const TARGET_EXIT: i32 = 42;

minilib::entry!(main);

fn exe(buf: &mut [u8; 64]) -> Option<&str> {
    let n = exe_path(buf).ok()?;
    core::str::from_utf8(&buf[..n]).ok()
}

fn target(buf: &mut [u8; 64]) -> i32 {
    let Some(path) = exe(buf) else {
        println!("exec-identity: FAIL exe_path");
        return 1;
    };
    println!("exec-identity: target exe={path}");
    TARGET_EXIT
}

fn main() -> i32 {
    let mut buf = [0u8; 64];
    if args().nth(1) == Some(b"target".as_slice()) {
        return target(&mut buf);
    }

    let Some(path) = exe(&mut buf) else {
        println!("exec-identity: FAIL exe_path");
        return 1;
    };
    println!("exec-identity: exe={path}");

    match fork() {
        Ok(0) => {
            let Some(path) = exe(&mut buf) else {
                println!("exec-identity: FAIL exe_path");
                return 1;
            };
            println!("exec-identity: child exe={path}");
            let errno = execve(TARGET, &[TARGET, "target"], &[]);
            println!("exec-identity: FAIL exec {errno}");
            1
        }
        Ok(pid) => {
            let status = match waitpid(pid) {
                Ok(status) => status,
                Err(e) => {
                    println!("exec-identity: FAIL waitpid {e}");
                    return 1;
                }
            };
            println!("exec-identity: parent waited status={status}");
            if status != TARGET_EXIT {
                println!("exec-identity: FAIL status {status}");
                return 1;
            }
            0
        }
        Err(e) => {
            println!("exec-identity: FAIL fork {e}");
            1
        }
    }
}
