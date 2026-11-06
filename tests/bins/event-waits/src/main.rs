//! This program checks that `poll` and `waitpid` wake on events.
//!
//! Each case runs in a fresh child process with fresh socket paths. Each
//! passed case prints `event-waits: <case> ok`. A failure prints
//! `event-waits: FAIL <case>: <error>` and exits 1 at once. After all cases,
//! the program prints `event-waits: ok`.

use std::error::Error;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kernel_abi::{ECHILD, EINTR, PollEvents, PollFd, Signal, WaitOptions};
use libc::{SIG_BLOCK, SIG_UNBLOCK, sigset_t};
use muffin_sys as sys;

type CaseResult = Result<(), Box<dyn Error>>;
type Case = fn(&str) -> CaseResult;

const CASES: &[(&str, Case)] = &[
    ("readiness", readiness),
    ("either socket", either_socket),
    ("duplicate entries", duplicate_entries),
    ("peer close hup", peer_close_hup),
    ("close nval", close_nval),
    ("replacement", replacement),
    ("two child waiters", two_child_waiters),
    ("signal eintr", signal_eintr),
    ("absolute deadline", absolute_deadline),
    ("empty deadline", empty_deadline),
];

/// The finite poll in `absolute_deadline` waits this long.
const POLL_TIMEOUT: Duration = Duration::from_millis(300);
/// A poll that extends its deadline after each notification returns only
/// after the writer stops, which is after this limit.
const WRITER_LIMIT: Duration = Duration::from_secs(2);
/// This is the latest acceptable return of the finite poll. It is well below
/// `WRITER_LIMIT` plus `POLL_TIMEOUT`.
const DEADLINE_LIMIT: Duration = Duration::from_millis(1500);
/// This is the pace of the data notifications in `absolute_deadline`.
const NOTIFY_INTERVAL: Duration = Duration::from_millis(10);
/// A signal that arrives before the waiter enters `poll` runs the handler
/// and does not interrupt the poll. Thus the sender repeats the signal at
/// this interval until the waiter reports a result.
const SIGNAL_INTERVAL: Duration = Duration::from_millis(50);
const SIGNAL_ATTEMPTS: u32 = 200;
const CHILD_EXIT_CODE: i32 = 7;

static HANDLED: AtomicUsize = AtomicUsize::new(0);

fn main() {
    for (index, (name, case)) in CASES.iter().enumerate() {
        let path = format!("/event-waits-{index}");
        if let Err(e) = run_in_child(case, &path) {
            println!("event-waits: FAIL {name}: {e}");
            std::process::exit(1);
        }
        println!("event-waits: {name} ok");
    }
    println!("event-waits: ok");
}

fn run_in_child(case: &Case, path: &str) -> CaseResult {
    let child = sys::fork().map_err(|e| format!("fork: {e:?}"))?;
    if child == 0 {
        let code = match case(path) {
            Ok(()) => 0,
            Err(e) => {
                println!("event-waits: case error: {e}");
                1
            }
        };
        std::process::exit(code);
    }
    let mut status = 0;
    // Safety: `status` is a live local.
    let reaped = unsafe { sys::waitpid(child, &raw mut status, WaitOptions::empty()) }
        .map_err(|e| format!("waitpid: {e:?}"))?;
    check(reaped == child, &format!("waitpid returned {reaped}"))?;
    check(status == 0, &format!("the case child status is {status}"))
}

fn check(ok: bool, what: &str) -> CaseResult {
    if ok { Ok(()) } else { Err(what.into()) }
}

fn panicked<T>(_: T) -> Box<dyn Error> {
    "a thread panicked".into()
}

/// Returns a connected socket pair on a fresh path. The first socket is the
/// accepted side.
fn pair(path: &str) -> Result<(UnixStream, UnixStream), Box<dyn Error>> {
    let listener = UnixListener::bind(path)?;
    thread::scope(|s| {
        let connector = s.spawn(|| UnixStream::connect(path));
        let (accepted, _) = listener.accept()?;
        let connected = connector.join().map_err(panicked)??;
        Ok((accepted, connected))
    })
}

fn poll_fds(fds: &mut [PollFd], timeout: Option<Duration>) -> Result<usize, String> {
    sys::poll(fds, timeout).map_err(|e| format!("poll: {e:?}"))
}

/// Polls `fds` without a timeout in a new thread. The thread reports through
/// the returned channel just before it enters `poll`. `act` runs after that
/// report.
fn poll_while(
    fds: &mut [PollFd],
    act: impl FnOnce() -> CaseResult,
) -> Result<usize, Box<dyn Error>> {
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::scope(|s| {
        let waiter = s.spawn(move || {
            let _ = ready_tx.send(());
            poll_fds(fds, None)
        });
        ready_rx.recv()?;
        act()?;
        Ok(waiter.join().map_err(panicked)??)
    })
}

/// Case 1: a blocked poll returns the requested readiness after another thread
/// sends data.
fn readiness(path: &str) -> CaseResult {
    let (a, mut b) = pair(path)?;
    let mut fds = [PollFd::new(a.as_raw_fd(), PollEvents::IN)];
    let count = poll_while(&mut fds, || Ok(b.write_all(b"x")?))?;
    check(count == 1, &format!("poll returned {count}"))?;
    check(
        fds[0].revents == PollEvents::IN,
        &format!("revents {:?}", fds[0].revents),
    )
}

/// Case 2: a poll over two independent sockets returns when one of them becomes
/// ready.
fn either_socket(path: &str) -> CaseResult {
    let (a1, _b1) = pair(&format!("{path}-a"))?;
    let (a2, mut b2) = pair(&format!("{path}-b"))?;
    let mut fds = [
        PollFd::new(a1.as_raw_fd(), PollEvents::IN),
        PollFd::new(a2.as_raw_fd(), PollEvents::IN),
    ];
    let count = poll_while(&mut fds, || Ok(b2.write_all(b"x")?))?;
    check(count == 1, &format!("poll returned {count}"))?;
    check(
        fds[0].revents.is_empty() && fds[1].revents == PollEvents::IN,
        &format!("revents {:?} {:?}", fds[0].revents, fds[1].revents),
    )
}

/// Case 3: duplicate entries keep separate result counts.
///
/// The send can occur while the first scan of `poll` runs. Then that scan
/// can see only the second entry as ready. Thus the blocked poll must report
/// at least one ready entry. A second scan after the send must count both.
fn duplicate_entries(path: &str) -> CaseResult {
    let (a, mut b) = pair(path)?;
    let entries = [
        PollFd::new(a.as_raw_fd(), PollEvents::IN),
        PollFd::new(a.as_raw_fd(), PollEvents::IN),
    ];
    let mut fds = entries;
    let count = poll_while(&mut fds, || Ok(b.write_all(b"x")?))?;
    let ready = fds.iter().filter(|fd| fd.revents == PollEvents::IN).count();
    let reported = fds.iter().filter(|fd| !fd.revents.is_empty()).count();
    check(
        count >= 1 && count == ready && count == reported,
        &format!("poll returned {count} with {fds:?}"),
    )?;
    let mut fds = entries;
    let count = poll_fds(&mut fds, Some(Duration::ZERO))?;
    check(count == 2, &format!("poll returned {count}"))?;
    check(
        fds.iter().all(|fd| fd.revents == PollEvents::IN),
        &format!("revents {:?} {:?}", fds[0].revents, fds[1].revents),
    )
}

/// Case 4: the close of the last peer reference releases a poll with `HUP`. The
/// poll requests no event.
fn peer_close_hup(path: &str) -> CaseResult {
    let (a, b) = pair(path)?;
    let mut fds = [PollFd::new(a.as_raw_fd(), PollEvents::empty())];
    let count = poll_while(&mut fds, || {
        drop(b);
        Ok(())
    })?;
    check(count == 1, &format!("poll returned {count}"))?;
    check(
        fds[0].revents == PollEvents::HUP,
        &format!("revents {:?}", fds[0].revents),
    )
}

/// Case 5: a concurrent close releases the wait with `NVAL`. No descriptor
/// replaces the closed one.
fn close_nval(path: &str) -> CaseResult {
    let (a, _b) = pair(path)?;
    let fd = a.into_raw_fd();
    let mut fds = [PollFd::new(fd, PollEvents::IN)];
    let count = poll_while(&mut fds, || {
        sys::close(fd).map_err(|e| format!("close: {e:?}"))?;
        Ok(())
    })?;
    check(count == 1, &format!("poll returned {count}"))?;
    check(
        fds[0].revents == PollEvents::NVAL,
        &format!("revents {:?}", fds[0].revents),
    )
}

/// Case 6: a replacement of the polled descriptor makes the wait observe the
/// replacement. The old description stays alive and becomes readable, but
/// only data on the replacement may end the wait. `dup2` replaces the
/// descriptor in one step, so the waiter never sees the number closed.
fn replacement(path: &str) -> CaseResult {
    let (old_a, mut old_b) = pair(&format!("{path}-old"))?;
    let (new_a, mut new_b) = pair(&format!("{path}-new"))?;
    let errno = |what: &'static str| move |e| format!("{what}: {e:?}");
    let fd = sys::dupfd(old_a.as_raw_fd(), 0).map_err(errno("dup old"))?;
    let new_sent = AtomicBool::new(false);
    let (ready_tx, ready_rx) = mpsc::channel();
    let revents = thread::scope(|s| -> Result<PollEvents, Box<dyn Error>> {
        let waiter = s.spawn(|| -> Result<PollEvents, String> {
            let _ = ready_tx.send(());
            let mut fds = [PollFd::new(fd, PollEvents::IN)];
            poll_fds(&mut fds, None)?;
            if !new_sent.load(Ordering::Acquire) {
                return Err(format!("woke before new data: {:?}", fds[0].revents));
            }
            Ok(fds[0].revents)
        });
        ready_rx.recv()?;
        let installed = sys::dup2(new_a.as_raw_fd(), fd).map_err(errno("dup2"))?;
        check(installed == fd, &format!("dup2 returned fd {installed}"))?;
        old_b.write_all(b"old")?;
        new_sent.store(true, Ordering::Release);
        new_b.write_all(b"new")?;
        Ok(waiter.join().map_err(panicked)??)
    })?;
    check(revents == PollEvents::IN, &format!("revents {revents:?}"))?;
    let mut buf = [0; 3];
    let read = sys::read(fd, &mut buf).map_err(|e| format!("read: {e:?}"))?;
    check(
        &buf[..read] == b"new",
        "the replacement did not return its data",
    )?;
    (&old_a).read_exact(&mut buf)?;
    check(&buf == b"old", "the old description lost its data")?;
    sys::close(fd).map_err(|e| format!("close: {e:?}"))?;
    Ok(())
}

/// Case 7: two waiters for one child produce one exit record and one `ECHILD`.
fn two_child_waiters(path: &str) -> CaseResult {
    let (release_a, mut release_b) = pair(path)?;
    let child = sys::fork().map_err(|e| format!("fork: {e:?}"))?;
    if child == 0 {
        let mut byte = [0];
        let _ = (&release_a).read(&mut byte);
        sys::exit(CHILD_EXIT_CODE);
    }
    drop(release_a);
    let (ready_tx, ready_rx) = mpsc::channel();
    let results = thread::scope(|s| -> Result<_, Box<dyn Error>> {
        let waiters: Vec<_> = (0..2)
            .map(|_| {
                let ready_tx = ready_tx.clone();
                s.spawn(move || {
                    let mut status = -1;
                    let _ = ready_tx.send(());
                    // Safety: `status` is a live local.
                    let reaped =
                        unsafe { sys::waitpid(child, &raw mut status, WaitOptions::empty()) };
                    (reaped, status)
                })
            })
            .collect();
        ready_rx.recv()?;
        ready_rx.recv()?;
        release_b.write_all(b"x")?;
        waiters
            .into_iter()
            .map(|waiter| waiter.join().map_err(panicked))
            .collect::<Result<Vec<_>, _>>()
    })?;
    let records = results
        .iter()
        .filter(|(reaped, status)| *reaped == Ok(child) && *status == CHILD_EXIT_CODE)
        .count();
    let missing = results
        .iter()
        .filter(|(reaped, _)| *reaped == Err(ECHILD))
        .count();
    check(
        records == 1 && missing == 1,
        &format!("waitpid results {results:?}"),
    )
}

extern "C" fn count_signal(_: Signal) {
    HANDLED.fetch_add(1, Ordering::SeqCst);
}

fn set_usr1_mask(how: libc::c_int) -> CaseResult {
    let usr1: sigset_t = Signal::Usr1.bit().bits();
    // Safety: both pointers are valid for the call.
    let result = unsafe { libc::pthread_sigmask(how, &usr1, std::ptr::null_mut()) };
    check(result == 0, &format!("pthread_sigmask returned {result}"))
}

/// Case 8: a signal releases an infinite wait with `EINTR`. Only the waiter
/// accepts the signal, so the process-directed signal reaches it.
fn signal_eintr(path: &str) -> CaseResult {
    let (a, _b) = pair(path)?;
    // Threads inherit the mask, so every later thread starts with the
    // signal blocked.
    set_usr1_mask(SIG_BLOCK)?;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let fd = a.as_raw_fd();
    thread::scope(|s| {
        s.spawn(move || {
            let prepared = sys::install_handler(Signal::Usr1, count_signal)
                .map_err(|e| format!("sigaction: {e:?}"))
                .and_then(|()| set_usr1_mask(SIG_UNBLOCK).map_err(|e| e.to_string()));
            if let Err(e) = prepared {
                let _ = done_tx.send(Err(e));
                return;
            }
            let _ = ready_tx.send(());
            let mut fds = [PollFd::new(fd, PollEvents::IN)];
            let result = sys::poll(&mut fds, None);
            // POSIX runs the handler before an interrupted call returns.
            let handled = HANDLED.load(Ordering::SeqCst);
            let _ = done_tx.send(Ok((result, handled)));
        });
        // A failed preparation drops `ready_tx`, so this receive ends.
        let _ = ready_rx.recv();
        for _ in 0..SIGNAL_ATTEMPTS {
            sys::kill(sys::getpid(), Signal::Usr1).map_err(|e| format!("kill: {e:?}"))?;
            match done_rx.recv_timeout(SIGNAL_INTERVAL) {
                Ok(result) => {
                    let (result, handled) = result?;
                    check(result == Err(EINTR), &format!("poll returned {result:?}"))?;
                    check(handled > 0, "poll returned EINTR before the handler ran")?;
                    return Ok(());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err("the signal did not release the poll".into())
    })
}

/// Case 9: a finite poll keeps its absolute deadline across repeated
/// notifications of an unrequested event.
fn absolute_deadline(path: &str) -> CaseResult {
    let (a, mut b) = pair(path)?;
    let polling = AtomicBool::new(true);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (count, revents, elapsed, writes) = thread::scope(|s| {
        let polling = &polling;
        let b = &mut b;
        let writer = s.spawn(move || -> Result<usize, String> {
            ready_rx.recv().map_err(|e| e.to_string())?;
            let start = Instant::now();
            let mut writes = 0;
            while polling.load(Ordering::Acquire) && start.elapsed() < WRITER_LIMIT {
                b.write_all(b"x").map_err(|e| e.to_string())?;
                writes += 1;
                thread::sleep(NOTIFY_INTERVAL);
            }
            Ok(writes)
        });
        let mut fds = [PollFd::new(a.as_raw_fd(), PollEvents::empty())];
        let _ = ready_tx.send(());
        let start = Instant::now();
        let count = poll_fds(&mut fds, Some(POLL_TIMEOUT));
        let elapsed = start.elapsed();
        polling.store(false, Ordering::Release);
        let writes = writer.join().map_err(panicked)?;
        Ok::<_, Box<dyn Error>>((count?, fds[0].revents, elapsed, writes?))
    })?;
    check(count == 0, &format!("poll returned {count}"))?;
    check(revents.is_empty(), &format!("revents {revents:?}"))?;
    check(writes >= 2, &format!("only {writes} notifications"))?;
    check(
        elapsed >= POLL_TIMEOUT && elapsed < DEADLINE_LIMIT,
        &format!("poll took {elapsed:?}"),
    )
}

/// Case 10: a finite empty poll returns zero at its deadline.
fn empty_deadline(_: &str) -> CaseResult {
    let timeout = Duration::from_millis(100);
    let start = Instant::now();
    let count = poll_fds(&mut [], Some(timeout))?;
    let elapsed = start.elapsed();
    check(count == 0, &format!("poll returned {count}"))?;
    check(elapsed >= timeout, &format!("poll took {elapsed:?}"))
}
