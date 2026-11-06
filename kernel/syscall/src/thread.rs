//! This module implements the thread syscalls: create, exit, join and self. It
//! also holds the per-process address wait queues of `SYS_ADDR_WAIT` and
//! `SYS_ADDR_WAKE`.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use kernel_abi::{
    EAGAIN, EDEADLK, EINTR, EINVAL, ESRCH, ETIMEDOUT, Errno, THREAD_ENTRY_STACK_REMAINDER,
    THREAD_STACK_ALIGN, ThreadExitFlags, ThreadId,
};
use tracing::{Level, instrument};

use crate::UserspacePtr;
use crate::access::{
    ExitOutcome, ParkEnd, SignalMaskAccess, ThreadAccess, ThreadStart, UnstartedThread, WakeQueue,
};

/// This type holds the live threads of one process. It also holds the exit
/// records of the joinable threads that end.
#[derive(Debug, Default)]
pub struct ThreadTable {
    live: BTreeSet<ThreadId>,
    exit_records: BTreeSet<ThreadId>,
}

/// This type describes what a thread exit leaves in its process.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum ThreadExit {
    LastThread,
    OthersLive,
}

/// This type describes the state of the thread that a join waits for.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum JoinState {
    /// The thread ends, and the join takes its exit record.
    Joined,
    Running,
    /// No live thread and no exit record has the id.
    Unknown,
}

impl ThreadTable {
    /// This method adds a thread before the thread runs for the first time.
    pub fn insert(&mut self, tid: ThreadId) {
        self.live.insert(tid);
    }

    /// This method removes every thread and every exit record.
    pub fn clear(&mut self) {
        self.live.clear();
        self.exit_records.clear();
    }

    /// This method keeps only `tid` and removes all exit records.
    pub fn reset_to(&mut self, tid: ThreadId) {
        self.clear();
        self.insert(tid);
    }

    fn exit(&mut self, tid: ThreadId, flags: ThreadExitFlags) -> ThreadExit {
        let was_live = self.live.remove(&tid);
        debug_assert!(was_live, "thread {tid} is not live");
        if self.live.is_empty() {
            self.exit_records.clear();
            return ThreadExit::LastThread;
        }
        if !flags.contains(ThreadExitFlags::DETACHED) {
            self.exit_records.insert(tid);
        }
        ThreadExit::OthersLive
    }

    fn join(&mut self, tid: ThreadId) -> JoinState {
        if self.exit_records.remove(&tid) {
            JoinState::Joined
        } else if self.live.contains(&tid) {
            JoinState::Running
        } else {
            JoinState::Unknown
        }
    }
}

/// This type represents one thread that waits in `SYS_ADDR_WAIT`.
#[derive(Debug, Default)]
pub struct AddrWaiter<Q> {
    woken: AtomicBool,
    queue: Q,
}

/// This type holds the `SYS_ADDR_WAIT` queues of one process. It has one
/// FIFO queue for each user address that has waiters.
#[derive(Debug)]
pub struct AddrWaitQueues<Q> {
    queues: BTreeMap<usize, VecDeque<Arc<AddrWaiter<Q>>>>,
}

impl<Q> AddrWaitQueues<Q> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queues: BTreeMap::new(),
        }
    }

    /// This method drops every waiter. A dropped waiter never wakes. Call it
    /// only for a process without threads that can still wait.
    pub fn clear(&mut self) {
        self.queues.clear();
    }
}

impl<Q> Default for AddrWaitQueues<Q> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Q: WakeQueue> AddrWaitQueues<Q> {
    fn enqueue(&mut self, addr: usize) -> Arc<AddrWaiter<Q>> {
        let waiter = Arc::new(AddrWaiter::default());
        self.queues
            .entry(addr)
            .or_default()
            .push_back(waiter.clone());
        waiter
    }

    /// This method removes up to `count` waiters of `addr` in FIFO order and
    /// marks them woken. The caller wakes their queues after it releases the lock.
    fn take(&mut self, addr: usize, count: usize) -> Vec<Arc<AddrWaiter<Q>>> {
        let Some(queue) = self.queues.get_mut(&addr) else {
            return Vec::new();
        };
        let taken: Vec<_> = queue.drain(..count.min(queue.len())).collect();
        if queue.is_empty() {
            self.queues.remove(&addr);
        }
        for waiter in &taken {
            waiter.woken.store(true, Ordering::Release);
        }
        taken
    }

    /// This method removes `waiter` from the queue of `addr`. It returns false
    /// when a wake takes the waiter first.
    fn cancel(&mut self, addr: usize, waiter: &Arc<AddrWaiter<Q>>) -> bool {
        let Some(queue) = self.queues.get_mut(&addr) else {
            return false;
        };
        let Some(index) = queue.iter().position(|w| Arc::ptr_eq(w, waiter)) else {
            return false;
        };
        queue.remove(index);
        if queue.is_empty() {
            self.queues.remove(&addr);
        }
        true
    }
}

/// This function implements `SYS_THREAD_CREATE`. It starts a thread of the
/// calling process at `entry` with `rdi = arg` and `rsp = stack_top`. The new
/// thread inherits the signal mask of the caller. The function returns the id
/// of the new thread.
///
/// # Errors
/// This function returns `EINVAL` when `entry` or `stack_top` is not a user
/// address. It returns `EINVAL` when `stack_top` modulo
/// [`THREAD_STACK_ALIGN`] differs from [`THREAD_ENTRY_STACK_REMAINDER`]. It
/// returns `EAGAIN` when the kernel cannot allocate the thread.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_thread_create<Cx: ThreadAccess + SignalMaskAccess>(
    cx: &Cx,
    entry: usize,
    arg: usize,
    stack_top: usize,
) -> Result<usize, Errno> {
    let entry = UserspacePtr::try_from_usize(entry)?;
    let stack_top = UserspacePtr::try_from_usize(stack_top)?;
    if stack_top.addr() % THREAD_STACK_ALIGN != THREAD_ENTRY_STACK_REMAINDER {
        return Err(EINVAL);
    }
    let thread = cx.create_thread(ThreadStart {
        entry,
        arg,
        stack_top,
        signal_mask: cx.signal_mask(),
    })?;
    let tid = thread.id();
    // The table must know the thread before the thread runs, because the
    // thread can exit at once.
    cx.threads().lock().insert(tid);
    cx.start_thread(thread);
    Ok(tid.as_u64() as usize)
}

/// This function implements `SYS_THREAD_EXIT` up to the point where the
/// calling thread ends. On success, the caller must end the calling thread
/// without a return to user mode.
///
/// When `unmap_len` is not zero, this function first unmaps
/// `[unmap_addr, unmap_addr + unmap_len)`. It keeps an exit record for a
/// later join, unless `flags` has `DETACHED`. It wakes the joiners. The exit
/// of the last thread ends the process with status 0 and drops every exit
/// record.
///
/// # Errors
/// This function returns `EINVAL` for an unknown `flags` bit. It returns
/// `EINVAL` for a range that `munmap` rejects. The thread continues to run
/// after an error.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_thread_exit<Cx: ThreadAccess>(
    cx: &Cx,
    flags: usize,
    unmap_addr: usize,
    unmap_len: usize,
) -> Result<(), Errno> {
    let flags = ThreadExitFlags::from_bits(flags).ok_or(EINVAL)?;
    if unmap_len != 0 {
        let addr = UserspacePtr::try_from_usize(unmap_addr)?;
        cx.unmap(addr, unmap_len).map_err(|_| EINVAL)?;
    }
    let exit = cx.threads().lock().exit(cx.current_thread(), flags);
    if exit == ThreadExit::LastThread {
        cx.set_exit_outcome(ExitOutcome::Exited(0));
    }
    cx.thread_exits().wake_all();
    Ok(())
}

/// This function implements `SYS_THREAD_JOIN`. It waits until the thread
/// `tid` of the calling process ends. Then it removes the exit record of
/// that thread.
///
/// # Errors
/// This function returns `EDEADLK` when `tid` is the caller. It returns
/// `ESRCH` when no live thread and no exit record of the process has `tid`.
/// It returns `ESRCH` also when another join takes the record. It returns
/// `EINTR` when a signal interrupts the wait.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_thread_join<Cx: ThreadAccess>(cx: &Cx, tid: ThreadId) -> Result<usize, Errno> {
    if tid == cx.current_thread() {
        return Err(EDEADLK);
    }
    let mut state = cx.threads().lock().join(tid);
    if state == JoinState::Running {
        // `state` holds the outcome, so the reason of the wake does not matter.
        let _ = cx.park(cx.thread_exits(), None, || {
            state = cx.threads().lock().join(tid);
            state != JoinState::Running
        });
    }
    match state {
        JoinState::Joined => Ok(0),
        JoinState::Running => Err(EINTR),
        JoinState::Unknown => Err(ESRCH),
    }
}

/// This function implements `SYS_THREAD_SELF`. It returns the id of the calling thread.
pub fn sys_thread_self<Cx: ThreadAccess>(cx: &Cx) -> usize {
    cx.current_thread().as_u64() as usize
}

/// This function implements `SYS_ADDR_WAIT`. It compares the `u32` at `addr`
/// with `expected`. When they are equal, it parks the calling thread in the
/// queue of `addr` until a `SYS_ADDR_WAKE` takes the thread. The compare and
/// the enqueue run under the lock that the wake takes. Thus a wake after the
/// compare always finds the waiter.
///
/// # Errors
/// This function returns `EINVAL` when `addr` is not 4-byte aligned. It
/// returns `EFAULT` when `addr` is not readable user memory. It returns
/// `EAGAIN` when the word is not `expected`. It returns `ETIMEDOUT` when
/// `timeout` elapses first. It returns `EINTR` when a signal interrupts the
/// wait.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_addr_wait<Cx: ThreadAccess>(
    cx: &Cx,
    addr: UserspacePtr<u32>,
    expected: u32,
    timeout: Option<Duration>,
) -> Result<usize, Errno> {
    if !addr.addr().is_multiple_of(align_of::<u32>()) {
        return Err(EINVAL);
    }
    let waiter = cx.with_user_word(addr, |load| {
        let mut queues = cx.addr_waiters().lock();
        if load() != expected {
            return Err(EAGAIN);
        }
        Ok(queues.enqueue(addr.addr()))
    })??;
    let errno = match cx.park(&waiter.queue, timeout, || {
        waiter.woken.load(Ordering::Acquire)
    }) {
        ParkEnd::Woken => return Ok(0),
        ParkEnd::TimedOut => ETIMEDOUT,
        ParkEnd::Interrupted => EINTR,
    };
    // A wake can take the waiter after the park ends and before this lock.
    // That wake counts this thread, so the wait succeeds.
    if cx.addr_waiters().lock().cancel(addr.addr(), &waiter) {
        Err(errno)
    } else {
        Ok(0)
    }
}

/// This function implements `SYS_ADDR_WAKE`. It wakes up to `count` threads
/// that wait on `addr` in the calling process, in FIFO order. It returns the
/// number of woken threads.
///
/// # Errors
/// This function returns `EINVAL` when `addr` is not 4-byte aligned.
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_addr_wake<Cx: ThreadAccess>(
    cx: &Cx,
    addr: UserspacePtr<u32>,
    count: usize,
) -> Result<usize, Errno> {
    if !addr.addr().is_multiple_of(align_of::<u32>()) {
        return Err(EINVAL);
    }
    let woken = cx.addr_waiters().lock().take(addr.addr(), count);
    for waiter in &woken {
        waiter.queue.wake_all();
    }
    Ok(woken.len())
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::cell::{Cell, RefCell};
    use core::sync::atomic::AtomicU32;

    use kernel_abi::{SigMaskHow, SigSet, Signal};
    use spin::Mutex;

    use super::*;
    use crate::signal::{SignalMask, sys_sigprocmask};

    const PAGE: usize = 4096;
    const STACK_TOP: usize = 0x7000_0000_0008;
    const ENTRY: usize = 0x20_1000;

    #[derive(Default)]
    struct TestQueue {
        wakes: Cell<usize>,
    }

    impl WakeQueue for TestQueue {
        fn wake_all(&self) {
            self.wakes.set(self.wakes.get() + 1);
        }
    }

    struct TestThread(ThreadId);

    impl UnstartedThread for TestThread {
        fn id(&self) -> ThreadId {
            self.0
        }
    }

    type Step = Box<dyn FnOnce(&TestProcess)>;

    /// This type simulates a process. Its `park` runs the next scripted step,
    /// which plays the other threads. Then `park` checks the wake condition once.
    #[derive(Default)]
    struct TestProcess {
        current: Cell<u64>,
        threads: Mutex<ThreadTable>,
        thread_exits: TestQueue,
        addr_waiters: Mutex<AddrWaitQueues<TestQueue>>,
        masks: RefCell<BTreeMap<u64, SignalMask>>,
        next_tid: Cell<u64>,
        allocation_fails: bool,
        created: RefCell<Vec<ThreadStart>>,
        started: RefCell<Vec<ThreadId>>,
        now: Cell<Duration>,
        interrupted: Cell<bool>,
        parks: Cell<usize>,
        during_park: RefCell<VecDeque<Step>>,
        outcome: Cell<Option<ExitOutcome>>,
        unmapped: RefCell<Vec<(usize, usize)>>,
        results: RefCell<Vec<(u64, Result<usize, Errno>)>>,
    }

    impl TestProcess {
        fn with_threads(tids: &[u64]) -> Self {
            let process = Self::default();
            for &raw in tids {
                process.threads.lock().insert(tid(raw));
            }
            process.current.set(tids[0]);
            process.next_tid.set(100);
            process
        }

        fn as_thread<R>(&self, raw: u64, f: impl FnOnce() -> R) -> R {
            let caller = self.current.replace(raw);
            let result = f();
            self.current.set(caller);
            result
        }

        fn on_park(&self, step: impl FnOnce(&TestProcess) + 'static) {
            self.during_park.borrow_mut().push_back(Box::new(step));
        }

        /// This method runs `syscall` as thread `raw` and records its result.
        fn run_as(&self, raw: u64, syscall: impl FnOnce() -> Result<usize, Errno>) {
            let result = self.as_thread(raw, syscall);
            self.results.borrow_mut().push((raw, result));
        }
    }

    impl ThreadAccess for TestProcess {
        type WakeQueue = TestQueue;
        type UnstartedThread = TestThread;

        fn current_thread(&self) -> ThreadId {
            tid(self.current.get())
        }

        fn threads(&self) -> &Mutex<ThreadTable> {
            &self.threads
        }

        fn thread_exits(&self) -> &TestQueue {
            &self.thread_exits
        }

        fn addr_waiters(&self) -> &Mutex<AddrWaitQueues<TestQueue>> {
            &self.addr_waiters
        }

        fn with_user_word<R>(
            &self,
            addr: UserspacePtr<u32>,
            f: impl FnOnce(&dyn Fn() -> u32) -> R,
        ) -> Result<R, Errno> {
            // Safety: each test passes the address of a live `AtomicU32`.
            let word = unsafe { AtomicU32::from_ptr(addr.addr() as *mut u32) };
            Ok(f(&|| word.load(Ordering::SeqCst)))
        }

        fn create_thread(&self, start: ThreadStart) -> Result<TestThread, Errno> {
            if self.allocation_fails {
                return Err(EAGAIN);
            }
            self.created.borrow_mut().push(start);
            let raw = self.next_tid.get();
            self.next_tid.set(raw + 1);
            Ok(TestThread(tid(raw)))
        }

        fn start_thread(&self, thread: TestThread) {
            self.started.borrow_mut().push(thread.0);
        }

        fn park(
            &self,
            _queue: &TestQueue,
            timeout: Option<Duration>,
            mut should_wake: impl FnMut() -> bool,
        ) -> ParkEnd {
            if should_wake() {
                return ParkEnd::Woken;
            }
            self.parks.set(self.parks.get() + 1);
            let step = self.during_park.borrow_mut().pop_front();
            if let Some(step) = step {
                step(self);
            }
            if should_wake() {
                return ParkEnd::Woken;
            }
            if self.interrupted.get() {
                return ParkEnd::Interrupted;
            }
            let timeout = timeout.expect("the thread would park forever");
            self.now.set(self.now.get() + timeout);
            ParkEnd::TimedOut
        }

        fn unmap(&self, addr: UserspacePtr<u8>, len: usize) -> Result<(), Errno> {
            if !addr.addr().is_multiple_of(PAGE) {
                return Err(EINVAL);
            }
            self.unmapped.borrow_mut().push((addr.addr(), len));
            Ok(())
        }

        fn set_exit_outcome(&self, outcome: ExitOutcome) {
            if self.outcome.get().is_none() {
                self.outcome.set(Some(outcome));
            }
        }
    }

    impl SignalMaskAccess for TestProcess {
        fn signal_mask(&self) -> SignalMask {
            self.masks
                .borrow()
                .get(&self.current.get())
                .copied()
                .unwrap_or_default()
        }

        fn set_signal_mask(&self, mask: SignalMask) {
            self.masks.borrow_mut().insert(self.current.get(), mask);
        }
    }

    fn tid(raw: u64) -> ThreadId {
        ThreadId::new(raw)
    }

    fn word_at(word: &AtomicU32) -> UserspacePtr<u32> {
        UserspacePtr::try_from_usize(word.as_ptr() as usize).unwrap()
    }

    fn exit_joinable(process: &TestProcess, raw: u64) {
        process
            .as_thread(raw, || sys_thread_exit(process, 0, 0, 0))
            .unwrap();
    }

    #[test]
    fn join_of_an_exited_thread_returns_at_once_and_takes_the_record() {
        let process = TestProcess::with_threads(&[1, 2]);
        exit_joinable(&process, 2);

        assert_eq!(sys_thread_join(&process, tid(2)), Ok(0));
        assert_eq!(process.parks.get(), 0, "the join does not park");
        assert_eq!(
            sys_thread_join(&process, tid(2)),
            Err(ESRCH),
            "the first join took the exit record"
        );
    }

    #[test]
    fn join_of_a_running_thread_parks_until_the_thread_exits() {
        let process = TestProcess::with_threads(&[1, 2]);
        process.on_park(|process| exit_joinable(process, 2));

        assert_eq!(sys_thread_join(&process, tid(2)), Ok(0));
        assert_eq!(process.parks.get(), 1, "the joiner parked once");
        assert_eq!(
            process.thread_exits.wakes.get(),
            1,
            "the exit woke the joiners"
        );
    }

    #[test]
    fn join_of_an_unknown_thread_fails_with_esrch() {
        let process = TestProcess::with_threads(&[1, 2]);
        assert_eq!(sys_thread_join(&process, tid(9)), Err(ESRCH));
    }

    #[test]
    fn join_of_the_caller_fails_with_edeadlk() {
        let process = TestProcess::with_threads(&[1, 2]);
        assert_eq!(sys_thread_join(&process, tid(1)), Err(EDEADLK));
    }

    #[test]
    fn interrupted_join_fails_with_eintr_and_keeps_the_thread_joinable() {
        let process = TestProcess::with_threads(&[1, 2]);
        process.interrupted.set(true);
        assert_eq!(sys_thread_join(&process, tid(2)), Err(EINTR));

        process.interrupted.set(false);
        exit_joinable(&process, 2);
        assert_eq!(sys_thread_join(&process, tid(2)), Ok(0));
    }

    #[test]
    fn detached_exit_leaves_no_record() {
        let process = TestProcess::with_threads(&[1, 2]);
        process
            .as_thread(2, || {
                sys_thread_exit(&process, ThreadExitFlags::DETACHED.bits(), 0, 0)
            })
            .unwrap();
        assert_eq!(sys_thread_join(&process, tid(2)), Err(ESRCH));
    }

    #[test]
    fn last_thread_exit_ends_the_process_with_status_zero() {
        let process = TestProcess::with_threads(&[1, 2]);

        exit_joinable(&process, 2);
        assert_eq!(
            process.outcome.get(),
            None,
            "the process lives while one thread runs"
        );

        exit_joinable(&process, 1);
        assert_eq!(process.outcome.get(), Some(ExitOutcome::Exited(0)));
    }

    #[test]
    fn thread_exit_with_unknown_flags_fails_and_the_thread_stays() {
        let process = TestProcess::with_threads(&[1]);
        assert_eq!(sys_thread_exit(&process, 0b10, 0, 0), Err(EINVAL));
        assert_eq!(process.outcome.get(), None, "the only thread did not end");
    }

    #[test]
    fn thread_exit_unmaps_the_range_first() {
        let process = TestProcess::with_threads(&[1, 2]);
        process
            .as_thread(2, || {
                sys_thread_exit(&process, ThreadExitFlags::DETACHED.bits(), 0x10_0000, PAGE)
            })
            .unwrap();
        assert_eq!(*process.unmapped.borrow(), [(0x10_0000, PAGE)]);
    }

    #[test]
    fn thread_exit_with_a_bad_range_fails_and_the_thread_stays() {
        let process = TestProcess::with_threads(&[1]);
        assert_eq!(
            sys_thread_exit(&process, 0, 0x10_0001, PAGE),
            Err(EINVAL),
            "munmap rejects an unaligned range"
        );
        assert_eq!(
            sys_thread_exit(&process, 0, usize::MAX, PAGE),
            Err(EINVAL),
            "a kernel address is no user range"
        );
        assert_eq!(process.outcome.get(), None, "the only thread did not end");
    }

    #[test]
    fn created_thread_inherits_the_mask_of_its_creator_and_is_joinable() {
        let process = TestProcess::with_threads(&[1]);
        sys_sigprocmask(&process, SigMaskHow::Block, Some(Signal::Usr1.bit()));

        let new = sys_thread_create(&process, ENTRY, 42, STACK_TOP).unwrap() as u64;

        let created = process.created.borrow();
        assert_eq!(created.len(), 1);
        assert_eq!(
            created[0].signal_mask.blocked(),
            Signal::Usr1.bit(),
            "the new thread blocks what its creator blocks"
        );
        assert_eq!(created[0].entry.addr(), ENTRY);
        assert_eq!(created[0].arg, 42);
        assert_eq!(created[0].stack_top.addr(), STACK_TOP);
        assert_eq!(*process.started.borrow(), [tid(new)]);

        process.on_park(move |process| exit_joinable(process, new));
        assert_eq!(sys_thread_join(&process, tid(new)), Ok(0));
    }

    #[test]
    fn mask_change_of_a_created_thread_leaves_its_creator_alone() {
        let process = TestProcess::with_threads(&[1]);
        let new = sys_thread_create(&process, ENTRY, 0, STACK_TOP).unwrap() as u64;

        process.as_thread(new, || {
            sys_sigprocmask(&process, SigMaskHow::SetMask, Some(SigSet::ALL))
        });
        assert_eq!(
            process.signal_mask(),
            SignalMask::default(),
            "the creator keeps its own mask"
        );
    }

    #[test]
    fn create_rejects_a_stack_top_without_the_entry_alignment() {
        let process = TestProcess::with_threads(&[1]);
        for stack_top in [STACK_TOP - 8, STACK_TOP + 1, STACK_TOP + 4] {
            assert_eq!(
                sys_thread_create(&process, ENTRY, 0, stack_top),
                Err(EINVAL),
                "stack_top {stack_top:#x}"
            );
        }
        assert!(process.created.borrow().is_empty());
    }

    #[test]
    fn create_rejects_kernel_addresses() {
        let process = TestProcess::with_threads(&[1]);
        let kernel = 0xFFFF_8000_0000_0008;
        assert_eq!(
            sys_thread_create(&process, kernel, 0, STACK_TOP),
            Err(EINVAL)
        );
        assert_eq!(sys_thread_create(&process, ENTRY, 0, kernel), Err(EINVAL));
        assert!(process.created.borrow().is_empty());
    }

    #[test]
    fn create_fails_with_eagain_when_the_kernel_lacks_memory() {
        let process = TestProcess {
            allocation_fails: true,
            ..TestProcess::with_threads(&[1])
        };
        assert_eq!(
            sys_thread_create(&process, ENTRY, 0, STACK_TOP),
            Err(EAGAIN)
        );
        assert!(process.started.borrow().is_empty());
    }

    #[test]
    fn thread_self_returns_the_caller() {
        let process = TestProcess::with_threads(&[1, 7]);
        assert_eq!(process.as_thread(7, || sys_thread_self(&process)), 7);
    }

    #[test]
    fn addr_wait_on_a_changed_value_returns_eagain_without_parking() {
        let process = TestProcess::with_threads(&[1]);
        let word = AtomicU32::new(5);

        assert_eq!(
            sys_addr_wait(&process, word_at(&word), 4, None),
            Err(EAGAIN)
        );
        assert_eq!(process.parks.get(), 0);
        assert_eq!(
            sys_addr_wake(&process, word_at(&word), usize::MAX),
            Ok(0),
            "the failed wait left no waiter"
        );
    }

    #[test]
    fn addr_wait_on_an_equal_value_parks_until_a_wake() {
        let process = TestProcess::with_threads(&[1, 2]);
        let word = AtomicU32::new(5);
        let addr = word_at(&word);
        process.on_park(move |process| process.run_as(2, || sys_addr_wake(process, addr, 1)));

        assert_eq!(sys_addr_wait(&process, addr, 5, None), Ok(0));
        assert_eq!(process.parks.get(), 1);
        assert_eq!(*process.results.borrow(), [(2, Ok(1))]);
    }

    #[test]
    fn addr_wake_wakes_at_most_count_waiters_in_fifo_order() {
        let process = TestProcess::with_threads(&[1, 2, 3, 4]);
        let word = AtomicU32::new(0);
        let addr = word_at(&word);
        let timeout = Some(Duration::from_millis(1));
        process.on_park(move |process| process.run_as(2, || sys_addr_wait(process, addr, 0, None)));
        process.on_park(move |process| {
            process.run_as(3, || sys_addr_wait(process, addr, 0, timeout));
        });
        process.on_park(move |process| process.run_as(4, || sys_addr_wake(process, addr, 2)));

        let first = sys_addr_wait(&process, addr, 0, None);

        assert_eq!(first, Ok(0), "the first waiter woke");
        assert_eq!(
            *process.results.borrow(),
            [(4, Ok(2)), (3, Err(ETIMEDOUT)), (2, Ok(0))],
            "the wake took the two oldest waiters and left the third"
        );
    }

    #[test]
    fn addr_wake_without_waiters_returns_zero() {
        let process = TestProcess::with_threads(&[1]);
        let word = AtomicU32::new(0);
        assert_eq!(sys_addr_wake(&process, word_at(&word), usize::MAX), Ok(0));
    }

    #[test]
    fn addr_wake_in_another_process_does_not_wake_the_waiter() {
        let a = TestProcess::with_threads(&[1]);
        let b = TestProcess::with_threads(&[7]);
        let word = AtomicU32::new(0);
        let addr = word_at(&word);
        a.on_park(move |a| a.run_as(7, || sys_addr_wake(&b, addr, usize::MAX)));

        assert_eq!(
            sys_addr_wait(&a, addr, 0, Some(Duration::from_millis(3))),
            Err(ETIMEDOUT)
        );
        assert_eq!(
            *a.results.borrow(),
            [(7, Ok(0))],
            "the wake in process B found no waiter"
        );
    }

    #[test]
    fn timed_addr_wait_returns_etimedout_after_the_timeout() {
        let process = TestProcess::with_threads(&[1]);
        let word = AtomicU32::new(0);
        let timeout = Duration::from_millis(5);

        assert_eq!(
            sys_addr_wait(&process, word_at(&word), 0, Some(timeout)),
            Err(ETIMEDOUT)
        );
        assert_eq!(process.now.get(), timeout);
        assert_eq!(
            sys_addr_wake(&process, word_at(&word), usize::MAX),
            Ok(0),
            "the timed out waiter left the queue"
        );
    }

    #[test]
    fn interrupted_addr_wait_returns_eintr() {
        let process = TestProcess::with_threads(&[1]);
        process.interrupted.set(true);
        let word = AtomicU32::new(0);

        assert_eq!(sys_addr_wait(&process, word_at(&word), 0, None), Err(EINTR));
        assert_eq!(
            sys_addr_wake(&process, word_at(&word), usize::MAX),
            Ok(0),
            "the interrupted waiter left the queue"
        );
    }

    #[test]
    fn misaligned_addr_is_einval() {
        let process = TestProcess::with_threads(&[1]);
        let words = [AtomicU32::new(0), AtomicU32::new(0)];
        let misaligned = UserspacePtr::try_from_usize(words[0].as_ptr() as usize + 1).unwrap();

        assert_eq!(sys_addr_wait(&process, misaligned, 0, None), Err(EINVAL));
        assert_eq!(sys_addr_wake(&process, misaligned, 1), Err(EINVAL));
    }
}
