use minilib::{
    EFAULT, EINVAL, ERANGE, SYS_EXE_PATH, SYS_GETCWD, args, env, exe_path, getpid, ret, syscall2,
};

use crate::check;

const EXE_PATH: &[u8] = b"/bin/posix";

const KERNEL_ADDR: usize = 0xFFFF_8000_0000_0000;

const FILL: u8 = 0xAA;

pub fn run() {
    check::group("process");

    identity();
    executable_path();
    working_directory();
    entry_stack();
}

fn identity() {
    let pid = getpid();
    check::require("process/getpid_first_manifest_entry", pid == 1);
    check::require("process/getpid_stable", getpid() == pid);
}

fn executable_path() {
    let mut buf = [FILL; 32];
    let written = check::unwrap_or_fail("process/exe_path_ok", exe_path(&mut buf));
    check::require("process/exe_path_len", written == EXE_PATH.len());
    check::require("process/exe_path_bytes", &buf[..EXE_PATH.len()] == EXE_PATH);
    check::require("process/exe_path_unterminated", buf[EXE_PATH.len()] == FILL);

    let mut short = [FILL; 4];
    check::expect_err("process/exe_path_short_buf", exe_path(&mut short), ERANGE);
    check::require("process/exe_path_short_buf_untouched", short[0] == FILL);

    let mut empty = [FILL; 0];
    check::expect_err("process/exe_path_zero_len", exe_path(&mut empty), EINVAL);

    check::expect_err(
        "process/exe_path_null_buf",
        ret(syscall2(SYS_EXE_PATH, 0, EXE_PATH.len())),
        EINVAL,
    );
    check::expect_err(
        "process/exe_path_kernel_buf",
        ret(syscall2(SYS_EXE_PATH, KERNEL_ADDR, EXE_PATH.len())),
        EFAULT,
    );
}

fn working_directory() {
    let mut buf = [FILL; 32];
    let addr = buf.as_mut_ptr() as usize;
    let returned = check::unwrap_or_fail(
        "process/getcwd_ok",
        ret(syscall2(SYS_GETCWD, addr, buf.len())),
    );
    check::require("process/getcwd_returns_buf_addr", returned == addr);
    check::require("process/getcwd_root", buf[0] == b'/');
    check::require("process/getcwd_nul_terminated", buf[1] == 0);
    check::require("process/getcwd_writes_nothing_beyond_nul", buf[2] == FILL);

    check::expect_err(
        "process/getcwd_short_buf",
        ret(syscall2(SYS_GETCWD, addr, 1)),
        ERANGE,
    );
    check::expect_err(
        "process/getcwd_zero_size",
        ret(syscall2(SYS_GETCWD, addr, 0)),
        EINVAL,
    );
    check::expect_err(
        "process/getcwd_null_buf",
        ret(syscall2(SYS_GETCWD, 0, buf.len())),
        EINVAL,
    );
    check::expect_err(
        "process/getcwd_kernel_buf",
        ret(syscall2(SYS_GETCWD, KERNEL_ADDR, buf.len())),
        EINVAL,
    );
}

fn entry_stack() {
    let mut argv = args();
    check::require(
        "process/argv0",
        argv.next().is_some_and(|arg| arg == EXE_PATH),
    );
    check::require("process/argv_count", argv.next().is_none());
    check::require("process/envp_empty", env().next().is_none());
}
