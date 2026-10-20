use alloc::vec;
use core::time::Duration;

use minilib::{
    CLOCK_MONOTONIC, E2BIG, EFAULT, EINVAL, ENAMETOOLONG, ENOENT, Errno, PATH_MAX, SYS_EXECVE,
    StrSlice, Timespec, clock_gettime, execve, syscall6,
};

use crate::check;

const MISSING: &str = "/nonexistent";
const SLOW_LIMIT: Duration = Duration::from_secs(1);

fn raw_execve(path: &str, argv_ptr: usize, argc: usize, envp_ptr: usize, envc: usize) -> Errno {
    // Safety: `path` is live for the call, and the argv and envp values are the
    // deliberately invalid inputs each check expects the kernel to reject.
    let raw = unsafe {
        syscall6(
            SYS_EXECVE,
            path.as_ptr() as usize,
            path.len(),
            argv_ptr,
            argc,
            envp_ptr,
            envc,
        )
    };
    Errno::from(-(raw as isize))
}

fn now() -> Duration {
    let mut ts = Timespec::default();
    check::unwrap_or_fail(
        "execve/clock_setup",
        clock_gettime(CLOCK_MONOTONIC, &mut ts),
    );
    check::unwrap_or_fail("execve/clock_setup", Duration::try_from(ts))
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
        end.saturating_sub(start) <= SLOW_LIMIT,
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
        end.saturating_sub(start) <= SLOW_LIMIT,
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
