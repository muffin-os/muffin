//! This program races syscalls that use user memory against `munmap` of that memory.
//!
//! Thread B maps and unmaps 64 KiB at the fixed address `REGION`. Thread A
//! gives a pointer into `REGION` to `read`, `poll` and `waitpid`. Each call
//! must succeed or fail with `EFAULT`. Another errno prints
//! `unmap-race: FAIL <op>: <errno>`. A kernel that touches the user page after
//! the unmap panics, and the harness detects the panic.

use std::ffi::c_void;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{process, ptr, thread};

use kernel_abi::{MapFlags, ProtFlags};

/// This is the fixed address of the raced mapping. It is below the region where the
/// kernel puts mappings without `MAP_FIXED`, and above the program image.
const REGION: usize = 0x4000_0000;
const REGION_LEN: usize = 64 * 1024;

/// This is the duration of the race.
const RACE_TIME: Duration = Duration::from_secs(2);

/// This is the maximum number of forked children. Each `waitpid` operation forks one child.
const MAX_FORKS: usize = 200;

/// This is the number of bytes that one `read` operation requests.
const READ_LEN: usize = 4096;

const SOCKET_PATH: &str = "/unmap-race.sock";

static FAILED: AtomicBool = AtomicBool::new(false);

fn fail(op: &str, errno: i32) {
    println!("unmap-race: FAIL {op}: {errno}");
    FAILED.store(true, Ordering::Relaxed);
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// This function accepts a successful result. It records a failure other than `EFAULT`.
fn accept(op: &str, result: isize) {
    if result < 0 {
        let errno = last_errno();
        if errno != libc::EFAULT {
            fail(op, errno);
        }
    }
}

/// Thread B maps `REGION`, writes the `pollfd` of `listener_fd` into it, and
/// unmaps it again. Thread B repeats these steps until `stop` becomes true.
fn remap(stop: &AtomicBool, listener_fd: i32) {
    while !stop.load(Ordering::Relaxed) {
        let mapped = muffin_sys::mmap(
            REGION,
            REGION_LEN,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE | MapFlags::ANONYMOUS | MapFlags::FIXED,
            0,
            0,
        );
        let base = match mapped {
            Ok(base) => base,
            Err(errno) => {
                fail("mmap", errno.into());
                return;
            }
        };
        let pollfd = libc::pollfd {
            fd: listener_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: only this thread maps and unmaps `REGION`. The page stays
        // mapped and writable here.
        unsafe { base.cast::<libc::pollfd>().write(pollfd) };
        // SAFETY: thread A holds no reference into the region, only an
        // address that it gives to the kernel.
        if let Err(errno) = unsafe { muffin_sys::munmap(base, REGION_LEN) } {
            fail("munmap", errno.into());
            return;
        }
    }
}

/// These are the counts of the operations of thread A.
#[derive(Default)]
struct Counts {
    reads: usize,
    polls: usize,
    waits: usize,
}

/// This function forks a child that sleeps 1 ms and exits with 3. It waits for the child with
/// the status pointer inside `REGION`.
fn wait_op() {
    // SAFETY: the child calls only `nanosleep` and `_exit`.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        fail("fork", last_errno());
        return;
    }
    if pid == 0 {
        let pause = libc::timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000,
        };
        // SAFETY: `pause` is a valid timespec, and `_exit` does not return.
        unsafe {
            libc::nanosleep(&raw const pause, ptr::null_mut());
            libc::_exit(3);
        }
    }
    // SAFETY: the kernel checks the status address.
    let waited = unsafe { libc::waitpid(pid, ptr::without_provenance_mut(REGION + 8), 0) };
    accept("waitpid", waited as isize);
    if waited < 0 {
        // The kernel can fail the status copy after it reaps the child, so
        // `ECHILD` is also a correct result here.
        let mut status = 0;
        // SAFETY: `status` is a live local.
        if unsafe { libc::waitpid(pid, &raw mut status, 0) } < 0 && last_errno() != libc::ECHILD {
            fail("waitpid reap", last_errno());
        }
    }
}

/// Thread A runs the three operations in turn until `stop` becomes true.
fn use_region(stop: &AtomicBool, exe: &mut File) -> Counts {
    let mut counts = Counts::default();
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = exe.seek(SeekFrom::Start(0)) {
            fail("seek", e.raw_os_error().unwrap_or(0));
        }
        // SAFETY: the kernel checks the buffer address.
        let read = unsafe {
            libc::read(
                exe.as_raw_fd(),
                ptr::without_provenance_mut::<c_void>(REGION + 64),
                READ_LEN,
            )
        };
        accept("read", read);
        counts.reads += 1;

        // SAFETY: the kernel checks the `pollfd` address.
        let polled = unsafe { libc::poll(ptr::without_provenance_mut(REGION), 1, 1) };
        accept("poll", polled as isize);
        counts.polls += 1;

        if counts.waits < MAX_FORKS {
            wait_op();
            counts.waits += 1;
        }
    }
    counts
}

fn main() {
    let mut exe = match std::env::current_exe().and_then(File::open) {
        Ok(file) => file,
        Err(e) => {
            println!("unmap-race: FAIL open: {e}");
            process::exit(1);
        }
    };
    let listener = match UnixListener::bind(SOCKET_PATH) {
        Ok(listener) => listener,
        Err(e) => {
            println!("unmap-race: FAIL bind: {e}");
            process::exit(1);
        }
    };
    let listener_fd = listener.as_raw_fd();
    let stop = AtomicBool::new(false);

    let counts = thread::scope(|scope| {
        scope.spawn(|| remap(&stop, listener_fd));
        let user = scope.spawn(|| use_region(&stop, &mut exe));
        let deadline = Instant::now() + RACE_TIME;
        while Instant::now() < deadline && !FAILED.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Relaxed);
        user.join()
    });
    let Ok(counts) = counts else {
        println!("unmap-race: FAIL thread A panicked");
        process::exit(1);
    };
    println!(
        "unmap-race: done reads={} polls={} waits={}",
        counts.reads, counts.polls, counts.waits
    );
    process::exit(i32::from(FAILED.load(Ordering::Relaxed)));
}
