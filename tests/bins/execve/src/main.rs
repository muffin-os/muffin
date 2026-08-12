#![no_std]
#![no_main]

use minilib::{
    ENOENT, ENOEXEC, SigAction, SigHandler, Signal, execve, exit, install_handler, open, sigaction,
    write,
};

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

extern "C" fn on_usr1(_signal: Signal) {}

minilib::entry!(main);

fn main() -> i32 {
    // Both error paths must return, proving the old image survives a failed
    // execve.
    if execve("/nonexistent", &["x"], &[]) != ENOENT {
        puts("execve-test: FAIL enoent\n");
        exit(1);
    }
    if execve("/data/notelf.txt", &["x"], &[]) != ENOEXEC {
        puts("execve-test: FAIL enoexec\n");
        exit(1);
    }

    // fds 0 to 2 are occupied, so this lands on fd 3. The replacement image
    // reads it back without opening anything.
    if open("/data/hello.txt") != Ok(3) {
        puts("execve-test: FAIL fd\n");
        exit(1);
    }

    if install_handler(Signal::Usr1, on_usr1).is_err() {
        puts("execve-test: FAIL handler\n");
        exit(1);
    }
    let ignore = SigAction {
        handler: SigHandler::IGNORE,
        ..SigAction::default()
    };
    if sigaction(Signal::Usr2, Some(&ignore), None).is_err() {
        puts("execve-test: FAIL ignore\n");
        exit(1);
    }

    puts("execve-test: before exec\n");
    let _ = execve(
        "/bin/exec-target",
        &["exec-target", "alpha", "beta"],
        &["KEY=value"],
    );
    puts("execve-test: FAIL exec returned\n");
    exit(1)
}
