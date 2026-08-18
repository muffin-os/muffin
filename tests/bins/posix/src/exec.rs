use alloc::vec;

use minilib::{
    CLOCK_MONOTONIC, E2BIG, EFAULT, EINVAL, ENAMETOOLONG, ENOENT, Errno, PATH_MAX, SYS_EXECVE,
    StrSlice, Timespec, clock_gettime, execve, syscall6,
};

use crate::check;

const MISSING: &str = "/nonexistent";
const SLOW_LIMIT_NANOS: i64 = 1_000_000_000;

fn raw_execve(path: &str, argv_ptr: usize, argc: usize, envp_ptr: usize, envc: usize) -> Errno {
    let raw = syscall6(
        SYS_EXECVE,
        path.as_ptr() as usize,
        path.len(),
        argv_ptr,
        argc,
        envp_ptr,
        envc,
    );
    Errno::from(-(raw as isize))
}

fn now() -> Timespec {
    let mut ts = Timespec::default();
    if clock_gettime(CLOCK_MONOTONIC, &mut ts).is_err() {
        check::fail("execve/clock_setup");
    }
    ts
}

fn elapsed_nanos(start: &Timespec, end: &Timespec) -> i64 {
    end.tv_sec
        .saturating_sub(start.tv_sec)
        .saturating_mul(1_000_000_000)
        .saturating_add(end.tv_nsec.saturating_sub(start.tv_nsec))
}

pub fn run() {
    check::group("execve");

    let arg = "x";
    let argv = [StrSlice::from(arg)];
    let argv_ptr = argv.as_ptr() as usize;

    let long = vec![b'a'; PATH_MAX + 1];
    let Ok(long_path) = core::str::from_utf8(&long) else {
        check::fail("execve/path_setup")
    };
    check::expect_errno(
        "execve/path_too_long",
        raw_execve(long_path, argv_ptr, 1, 0, 0),
        ENAMETOOLONG,
    );

    let start = now();
    check::expect_errno(
        "execve/argc_overflow",
        raw_execve(MISSING, argv_ptr, 1 << 36, 0, 0),
        E2BIG,
    );
    let end = now();
    check::require(
        "execve/argc_dos_slow",
        elapsed_nanos(&start, &end) <= SLOW_LIMIT_NANOS,
    );

    let empty = vec![unsafe { StrSlice::from_raw(0, 0) }; 10_000];
    check::expect_errno(
        "execve/many_empty_args_within_budget",
        raw_execve(MISSING, empty.as_ptr() as usize, empty.len(), 0, 0),
        ENOENT,
    );

    let start = now();
    check::expect_errno(
        "execve/envc_overflow",
        raw_execve(MISSING, argv_ptr, 1, argv_ptr, 1 << 36),
        E2BIG,
    );
    let end = now();
    check::require(
        "execve/envc_dos_slow",
        elapsed_nanos(&start, &end) <= SLOW_LIMIT_NANOS,
    );

    check::expect_errno("execve/null_argv", raw_execve(MISSING, 0, 1, 0, 0), EFAULT);
    check::expect_errno(
        "execve/null_argv_zero_count",
        raw_execve(MISSING, 0, 0, 0, 0),
        ENOENT,
    );

    let nul = "a\0b";
    let nul_argv = [StrSlice::from(nul)];
    check::expect_errno(
        "execve/embedded_nul",
        raw_execve(MISSING, nul_argv.as_ptr() as usize, 1, 0, 0),
        EINVAL,
    );

    let blob = vec![b'a'; 2000];
    let fat = vec![unsafe { StrSlice::from_raw(blob.as_ptr() as usize, blob.len()) }; 100];
    check::expect_errno(
        "execve/arg_bytes_over_budget",
        raw_execve(MISSING, fat.as_ptr() as usize, fat.len(), 0, 0),
        E2BIG,
    );

    check::expect_errno("execve/still_works", execve(MISSING, &["x"], &[]), ENOENT);
}
