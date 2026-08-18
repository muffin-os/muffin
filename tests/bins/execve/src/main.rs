#![no_std]
#![no_main]

use minilib::{
    ENOENT, ENOEXEC, SigAction, SigHandler, Signal, execve, install_handler, open, println,
    sigaction,
};

extern "C" fn on_usr1(_signal: Signal) {}

minilib::entry!(main);

fn main() -> i32 {
    // Both error paths must return, proving the old image survives a failed
    // execve.
    if execve("/nonexistent", &["x"], &[]) != ENOENT {
        println!("execve-test: FAIL enoent");
        return 1;
    }
    if execve("", &["x"], &[]) != ENOENT {
        println!("execve-test: FAIL empty");
        return 1;
    }
    if execve("/data/notelf.txt", &["x"], &[]) != ENOEXEC {
        println!("execve-test: FAIL enoexec");
        return 1;
    }

    // fds 0 to 2 are occupied, so this lands on fd 3. The replacement image
    // reads it back without opening anything.
    if open("/data/hello.txt") != Ok(3) {
        println!("execve-test: FAIL fd");
        return 1;
    }

    if install_handler(Signal::Usr1, on_usr1).is_err() {
        println!("execve-test: FAIL handler");
        return 1;
    }
    let ignore = SigAction {
        handler: SigHandler::IGNORE,
        ..SigAction::default()
    };
    if sigaction(Signal::Usr2, Some(&ignore), None).is_err() {
        println!("execve-test: FAIL ignore");
        return 1;
    }

    println!("execve-test: before exec");
    let _ = execve(
        "/bin/exec-target",
        &["exec-target", "alpha", "beta"],
        &["KEY=value"],
    );
    println!("execve-test: FAIL exec returned");
    1
}
