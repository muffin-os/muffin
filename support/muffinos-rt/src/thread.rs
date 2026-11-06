//! This module implements pthread threads on the muffin thread syscalls.
//!
//! `pthread_t` is the kernel tid. Each thread has a [`ThreadBlock`].
//! The main thread has a static block.
//! A thread of `pthread_create` has a block on the heap and an `mmap` stack.
//! `pthread_join` and `pthread_detach` find a block by its tid in [`REGISTRY`].
//! The [`Lifecycle`] word of the block decides which thread frees the block
//! and the stack.

mod cond;
mod key;
mod mutex;
mod syscall;

use core::cell::Cell;
use core::ffi::{c_char, c_int, c_void};
use core::iter;
use core::ptr::{self, NonNull};
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed};
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize};

use kernel_abi::{
    EAGAIN, EDEADLK, EINVAL, ERANGE, ESRCH, Errno, MapFlags, ProtFlags,
    THREAD_ENTRY_STACK_REMAINDER, ThreadExitFlags,
};
use libc::{
    PTHREAD_CREATE_DETACHED, PTHREAD_CREATE_JOINABLE, PTHREAD_STACK_MIN, pthread_attr_t, pthread_t,
};

use self::mutex::Locked;
use crate::errno::error_code;
use crate::mem::malloc;
use crate::process::PAGE_SIZE;

/// This constant is the stack size of a thread whose attribute names no size.
const DEFAULT_STACK_SIZE: usize = 1 << 20;

/// This constant is the owner word of an `ERRORCHECK` mutex that no thread
/// holds. The function [`enter`] checks that no thread has this id.
const NO_OWNER: u32 = 0;

#[thread_local]
static TID: Cell<pthread_t> = Cell::new(0);

#[thread_local]
static CURRENT: Cell<*const ThreadBlock> = Cell::new(ptr::null());

type StartRoutine = extern "C" fn(*mut c_void) -> *mut c_void;

/// This struct is the stack that `pthread_create` maps for a thread. The
/// mapping starts with one `PROT_NONE` guard page. An overflow then kills the
/// process with `SIGSEGV`. `base` and `len` name the full mapping. The full
/// mapping includes the guard page.
#[derive(Clone, Copy)]
struct Stack {
    base: *mut u8,
    len: usize,
}

impl Stack {
    /// This constant is the size in bytes of the guard page at the base of the
    /// mapping.
    const GUARD_SIZE: usize = PAGE_SIZE;

    /// This function fails with `EAGAIN` when the kernel cannot map the stack
    /// or the guard.
    fn map(size: usize) -> Result<Self, Errno> {
        let len = size
            .max(PTHREAD_STACK_MIN)
            .checked_next_multiple_of(PAGE_SIZE)
            .and_then(|usable| usable.checked_add(Self::GUARD_SIZE))
            .ok_or(EAGAIN)?;
        let base = muffin_sys::mmap(
            0,
            len,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE | MapFlags::ANONYMOUS,
            0,
            0,
        )
        .map_err(|_| EAGAIN)?;
        let stack = Self { base, len };
        // SAFETY: no reference into the new mapping exists.
        if unsafe { muffin_sys::mprotect(base, Self::GUARD_SIZE, ProtFlags::NONE) }.is_err() {
            // SAFETY: no thread uses the new stack.
            unsafe { stack.unmap() };
            return Err(EAGAIN);
        }
        Ok(stack)
    }

    fn top(self) -> usize {
        self.base as usize + self.len - THREAD_ENTRY_STACK_REMAINDER
    }

    /// # Safety
    ///
    /// No thread may use the stack.
    unsafe fn unmap(self) {
        let unmapped = unsafe { muffin_sys::munmap(self.base, self.len) };
        debug_assert!(
            unmapped.is_ok(),
            "munmap of a thread stack failed: {unmapped:?}"
        );
    }
}

/// This enum tells which thread frees the block and the stack of a thread.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    /// The thread runs and is not detached.
    Joinable,
    /// Nobody joins the thread, so the thread frees its own block and stack.
    Detached,
    /// The thread no longer runs and is joinable. The kernel keeps its exit
    /// record. `pthread_join` or `pthread_detach` frees the block and the
    /// stack.
    Exited,
}

impl Lifecycle {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Joinable,
            1 => Self::Detached,
            2 => Self::Exited,
            _ => unreachable!("lifecycle word {raw} holds no `Lifecycle`"),
        }
    }
}

struct AtomicLifecycle(AtomicU32);

impl AtomicLifecycle {
    const fn new(lifecycle: Lifecycle) -> Self {
        Self(AtomicU32::new(lifecycle as u32))
    }

    fn load(&self) -> Lifecycle {
        Lifecycle::from_raw(self.0.load(Acquire))
    }

    /// This function moves from `from` to `to`. It returns the current value
    /// when the move fails.
    fn transition(&self, from: Lifecycle, to: Lifecycle) -> Result<(), Lifecycle> {
        self.0
            .compare_exchange(from as u32, to as u32, AcqRel, Acquire)
            .map(drop)
            .map_err(Lifecycle::from_raw)
    }
}

enum Origin {
    /// An `exec` or `fork` call starts this thread.
    /// Its block is static, and its stack belongs to the kernel.
    Main,
    /// This variant is a thread that `pthread_create` creates.
    Created {
        start: StartRoutine,
        arg: *mut c_void,
        stack: Stack,
    },
}

struct ThreadBlock {
    /// A thread writes this field under the `REGISTRY` mutex before it links
    /// the block.
    tid: AtomicUsize,
    /// This field points to the next block in `REGISTRY`. A thread changes it
    /// only under the `REGISTRY` mutex.
    next: AtomicPtr<ThreadBlock>,
    lifecycle: AtomicLifecycle,
    /// This field holds the value for `pthread_join`. The thread writes it
    /// before the move to `Exited`.
    result: AtomicPtr<c_void>,
    origin: Origin,
}

// SAFETY: the runtime does not access `arg` and `stack`. The new thread
// passes `arg` to `start`. The thread that frees the block unmaps `stack`.
// The other fields are atomics.
unsafe impl Sync for ThreadBlock {}

impl ThreadBlock {
    const fn new(origin: Origin, lifecycle: Lifecycle) -> Self {
        Self {
            tid: AtomicUsize::new(0),
            next: AtomicPtr::new(ptr::null_mut()),
            lifecycle: AtomicLifecycle::new(lifecycle),
            result: AtomicPtr::new(ptr::null_mut()),
            origin,
        }
    }

    fn allocate(origin: Origin, lifecycle: Lifecycle) -> Option<NonNull<Self>> {
        let block = NonNull::new(malloc(size_of::<Self>()).cast::<Self>())?;
        // SAFETY: `malloc` returns 16-byte aligned memory for one block.
        unsafe { block.write(Self::new(origin, lifecycle)) };
        Some(block)
    }

    fn stack(&self) -> Option<Stack> {
        match self.origin {
            Origin::Main => None,
            Origin::Created { stack, .. } => Some(stack),
        }
    }

    /// This function frees a block of `pthread_create`. It does not free the
    /// static main block.
    ///
    /// # Safety
    ///
    /// The caller must not use any reference to the block after the call.
    unsafe fn free(block: *const Self) {
        if let Origin::Created { .. } = unsafe { &(*block).origin } {
            unsafe { crate::mem::free(block.cast_mut().cast()) };
        }
    }

    /// This function unmaps the stack and frees the block of a thread that
    /// does not run.
    ///
    /// # Safety
    ///
    /// The caller must satisfy the contract of [`Self::free`].
    unsafe fn reclaim(block: *const Self) {
        if let Some(stack) = unsafe { (*block).stack() } {
            unsafe { stack.unmap() };
        }
        unsafe { Self::free(block) };
    }
}

static MAIN_BLOCK: ThreadBlock = ThreadBlock::new(Origin::Main, Lifecycle::Joinable);

/// This registry holds the threads that a `pthread_t` can name.
/// Their blocks remain allocated, and no `pthread_join` call owns them.
struct Registry {
    head: AtomicPtr<ThreadBlock>,
}

impl Registry {
    fn push(&self, block: &ThreadBlock) {
        block.next.store(self.head.load(Relaxed), Relaxed);
        self.head.store(ptr::from_ref(block).cast_mut(), Relaxed);
    }

    fn blocks(&self) -> impl Iterator<Item = &ThreadBlock> {
        let mut next = self.head.load(Relaxed);
        iter::from_fn(move || {
            // SAFETY: a linked block stays live until a thread unlinks it. The
            // caller holds the `REGISTRY` mutex.
            let block = unsafe { next.as_ref() }?;
            next = block.next.load(Relaxed);
            Some(block)
        })
    }

    fn find(&self, tid: pthread_t) -> Option<&ThreadBlock> {
        self.blocks().find(|block| block.tid.load(Relaxed) == tid)
    }

    fn unlink(&self, target: &ThreadBlock) {
        let mut link = &self.head;
        loop {
            let block = link.load(Relaxed);
            if block.is_null() {
                return;
            }
            if ptr::eq(block, target) {
                link.store(target.next.load(Relaxed), Relaxed);
                return;
            }
            // SAFETY: see `blocks`.
            link = unsafe { &(*block).next };
        }
    }
}

static REGISTRY: Locked<Registry> = Locked::new(Registry {
    head: AtomicPtr::new(ptr::null_mut()),
});

/// This function caches the tid and the block of the calling thread. It
/// returns the tid.
fn enter(block: &ThreadBlock) -> pthread_t {
    let tid = syscall::thread_self();
    assert!(
        u32::try_from(tid).is_ok_and(|id| id != NO_OWNER),
        "tid {tid} does not fit the owner word of a mutex"
    );
    TID.set(tid);
    CURRENT.set(block);
    tid
}

/// This function returns the low 32 bits of the tid of the calling thread.
/// This value is the owner word of an `ERRORCHECK` mutex. The function
/// [`enter`] checks that the tid has no other bits.
fn owner_id() -> u32 {
    TID.get() as u32
}

/// This function registers the main thread. `_start` calls it before `main`.
pub(crate) extern "C" fn init_main_thread() {
    let tid = enter(&MAIN_BLOCK);
    MAIN_BLOCK.tid.store(tid, Relaxed);
    REGISTRY.with(|registry| registry.push(&MAIN_BLOCK));
}

/// This function caches the tid of the only thread in the child of `fork`.
/// The child runs a new thread. This thread has a copy of the TLS of the
/// thread that calls `fork` in the parent. The `REGISTRY` mutex can be a copy
/// of a mutex that a thread of the parent holds. The block therefore gets its
/// new tid without the mutex.
pub(crate) fn refresh_after_fork() {
    // SAFETY: `enter` sets `CURRENT` to a block that lives while its thread runs.
    let block = unsafe { &*CURRENT.get() };
    let tid = enter(block);
    block.tid.store(tid, Relaxed);
}

extern "C" fn thread_entry(block: *const ThreadBlock) -> ! {
    // SAFETY: `spawn` passes a live block, and only this thread frees it
    // before the kernel ends this thread.
    let block = unsafe { &*block };
    enter(block);
    let Origin::Created { start, arg, .. } = block.origin else {
        unreachable!("only `spawn` starts threads at `thread_entry`")
    };
    exit_thread(start(arg))
}

/// This function runs the key destructors, publishes `result` and ends the
/// calling thread.
fn exit_thread(result: *mut c_void) -> ! {
    key::run_destructors();
    let block = CURRENT.get();
    // SAFETY: `enter` sets `CURRENT` to the block of this thread, and only the
    // move to `Exited` lets another thread free it.
    let current = unsafe { &*block };
    current.result.store(result, Relaxed);
    match current
        .lifecycle
        .transition(Lifecycle::Joinable, Lifecycle::Exited)
    {
        Ok(()) => unsafe { syscall::thread_exit(ThreadExitFlags::empty(), None) },
        Err(Lifecycle::Detached) => {
            REGISTRY.with(|registry| registry.unlink(current));
            let stack = current.stack();
            // SAFETY: the block has no link in `REGISTRY`, so no other thread can reach it.
            unsafe { ThreadBlock::free(block) };
            // SAFETY: the kernel unmaps the stack after this thread stops using it.
            unsafe { syscall::thread_exit(ThreadExitFlags::DETACHED, stack) }
        }
        Err(lifecycle @ (Lifecycle::Joinable | Lifecycle::Exited)) => {
            unreachable!("only the thread itself moves to `Exited`, found {lifecycle:?}")
        }
    }
}

/// This struct is the layout of `pthread_attr_t`. All-zero bytes describe a
/// joinable thread with the default stack size.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Attr {
    stack_size: usize,
    detach_state: c_int,
}

const _: () = assert!(size_of::<Attr>() == size_of::<pthread_attr_t>());
const _: () = assert!(align_of::<Attr>() <= align_of::<pthread_attr_t>());

impl Attr {
    fn stack_size(self) -> usize {
        if self.stack_size == 0 {
            DEFAULT_STACK_SIZE
        } else {
            self.stack_size
        }
    }

    fn lifecycle(self) -> Lifecycle {
        if self.detach_state == PTHREAD_CREATE_DETACHED {
            Lifecycle::Detached
        } else {
            Lifecycle::Joinable
        }
    }

    /// # Safety
    ///
    /// `attr` must point to an attribute with initialization from
    /// `pthread_attr_init`. The attribute must outlive `'a`.
    unsafe fn from_raw<'a>(attr: *mut pthread_attr_t) -> &'a mut Self {
        unsafe { &mut *attr.cast::<Self>() }
    }
}

fn spawn(attr: Attr, start: StartRoutine, arg: *mut c_void) -> Result<pthread_t, Errno> {
    let stack = Stack::map(attr.stack_size())?;
    let origin = Origin::Created { start, arg, stack };
    let Some(block) = ThreadBlock::allocate(origin, attr.lifecycle()) else {
        // SAFETY: no thread runs on the new stack.
        unsafe { stack.unmap() };
        return Err(EAGAIN);
    };
    let block = block.as_ptr().cast_const();
    // A detached thread can end and free its block as soon as the mutex is
    // free. The block therefore gets its tid and its link under the same mutex
    // hold as the create.
    let created = REGISTRY.with(|registry| {
        // SAFETY: the stack is new, and the block lives until the thread or
        // its joiner frees it.
        let tid = unsafe { syscall::thread_create(thread_entry, block, stack.top()) }?;
        // SAFETY: the new thread frees the block only under the mutex.
        let created = unsafe { &*block };
        created.tid.store(tid, Relaxed);
        registry.push(created);
        Ok(tid)
    });
    if created.is_err() {
        // SAFETY: a failed create starts no thread, so nothing uses the block and the stack.
        unsafe { ThreadBlock::reclaim(block) };
    }
    created
}

/// This function fails with `EAGAIN` when the kernel cannot map the stack or
/// start the thread.
///
/// # Safety
///
/// `thread` must be valid for one `pthread_t` write, and `attr` must be null
/// or point to an attribute with initialization from `pthread_attr_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_create(
    thread: *mut pthread_t,
    attr: *const pthread_attr_t,
    start: StartRoutine,
    arg: *mut c_void,
) -> c_int {
    let attr = unsafe { attr.cast::<Attr>().as_ref() }
        .copied()
        .unwrap_or_default();
    error_code(spawn(attr, start, arg).map(|tid| unsafe { thread.write(tid) }))
}

fn join(tid: pthread_t) -> Result<*mut c_void, Errno> {
    if tid == pthread_self() {
        return Err(EDEADLK);
    }
    let block = REGISTRY.with(|registry| {
        let block = registry.find(tid).ok_or(ESRCH)?;
        if block.lifecycle.load() == Lifecycle::Detached {
            return Err(EINVAL);
        }
        // The unlink claims the block. No other join or detach finds it.
        registry.unlink(block);
        Ok(ptr::from_ref(block))
    })?;
    syscall::thread_join(tid)?;
    // SAFETY: this thread owns the block through the claim, and the joined
    // thread does not run any more.
    let joined = unsafe { &*block };
    let lifecycle = joined.lifecycle.load();
    debug_assert_eq!(lifecycle, Lifecycle::Exited);
    let result = joined.result.load(Relaxed);
    unsafe { ThreadBlock::reclaim(block) };
    Ok(result)
}

/// This function fails with `ESRCH` when no joinable thread has the id
/// `thread`. It fails with `EINVAL` for a detached thread and with `EDEADLK`
/// for the calling thread.
///
/// # Safety
///
/// `result` must be null or valid for one pointer write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_join(thread: pthread_t, result: *mut *mut c_void) -> c_int {
    error_code(join(thread).map(|value| {
        if !result.is_null() {
            unsafe { result.write(value) };
        }
    }))
}

fn detach(tid: pthread_t) -> Result<(), Errno> {
    let exited = REGISTRY.with(|registry| {
        let block = registry.find(tid).ok_or(ESRCH)?;
        match block
            .lifecycle
            .transition(Lifecycle::Joinable, Lifecycle::Detached)
        {
            Ok(()) => Ok(None),
            Err(Lifecycle::Exited) => {
                registry.unlink(block);
                Ok(Some(ptr::from_ref(block)))
            }
            Err(Lifecycle::Detached) => Err(EINVAL),
            Err(Lifecycle::Joinable) => unreachable!("the transition from `Joinable` failed"),
        }
    })?;
    if let Some(block) = exited {
        // The join drops the exit record that the kernel keeps for the thread.
        syscall::thread_join(tid)?;
        // SAFETY: this thread owns the block through the unlink, and the joined
        // thread does not run any more.
        unsafe { ThreadBlock::reclaim(block) };
    }
    Ok(())
}

/// This function fails with `ESRCH` when no thread has the id `thread`. It
/// fails with `EINVAL` for a detached thread.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_detach(thread: pthread_t) -> c_int {
    error_code(detach(thread))
}

/// This function ends the calling thread with `result` for `pthread_join`.
/// The process exits with status 0 when the calling thread is its last thread.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_exit(result: *mut c_void) -> ! {
    exit_thread(result)
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_self() -> pthread_t {
    TID.get()
}

/// # Safety
///
/// `attr` must be valid for one `pthread_attr_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_attr_init(attr: *mut pthread_attr_t) -> c_int {
    let defaults = Attr {
        stack_size: DEFAULT_STACK_SIZE,
        detach_state: PTHREAD_CREATE_JOINABLE,
    };
    unsafe { attr.cast::<Attr>().write(defaults) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_destroy(_: *mut pthread_attr_t) -> c_int {
    0
}

/// This function fails with `EINVAL` for a size below `PTHREAD_STACK_MIN`.
/// `pthread_create` rounds the size up to a page.
///
/// # Safety
///
/// `attr` must point to an attribute with initialization from
/// `pthread_attr_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_attr_setstacksize(
    attr: *mut pthread_attr_t,
    size: usize,
) -> c_int {
    if size < PTHREAD_STACK_MIN {
        return EINVAL.into();
    }
    unsafe { Attr::from_raw(attr) }.stack_size = size;
    0
}

/// # Safety
///
/// `attr` must point to an attribute with initialization from
/// `pthread_attr_init`, and `size` must be valid for one `usize` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_attr_getstacksize(
    attr: *const pthread_attr_t,
    size: *mut usize,
) -> c_int {
    let attr = unsafe { *attr.cast::<Attr>() };
    unsafe { size.write(attr.stack_size()) };
    0
}

/// This function fails with `EINVAL` for a value other than
/// `PTHREAD_CREATE_JOINABLE` and `PTHREAD_CREATE_DETACHED`.
///
/// # Safety
///
/// `attr` must point to an attribute with initialization from
/// `pthread_attr_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_attr_setdetachstate(
    attr: *mut pthread_attr_t,
    state: c_int,
) -> c_int {
    if state != PTHREAD_CREATE_JOINABLE && state != PTHREAD_CREATE_DETACHED {
        return EINVAL.into();
    }
    unsafe { Attr::from_raw(attr) }.detach_state = state;
    0
}

/// # Safety
///
/// `attr` must point to an attribute with initialization from
/// `pthread_attr_init`, and `state` must be valid for one `c_int` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_attr_getdetachstate(
    attr: *const pthread_attr_t,
    state: *mut c_int,
) -> c_int {
    let attr = unsafe { *attr.cast::<Attr>() };
    unsafe { state.write(attr.detach_state) };
    0
}

/// This function writes the size of the `PROT_NONE` guard page at the base of
/// each thread stack that `pthread_create` maps.
///
/// # Safety
///
/// `size` must be valid for one `usize` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_attr_getguardsize(
    _: *const pthread_attr_t,
    size: *mut usize,
) -> c_int {
    unsafe { size.write(Stack::GUARD_SIZE) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_setname_np(_: pthread_t, _: *const c_char) -> c_int {
    0
}

/// This function writes the empty name, because `pthread_setname_np` stores
/// no name.
///
/// # Safety
///
/// `name` must be valid for `len` bytes of writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_getname_np(_: pthread_t, name: *mut c_char, len: usize) -> c_int {
    if len == 0 {
        return ERANGE.into();
    }
    unsafe { name.write(0) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn sched_yield() -> c_int {
    0
}
