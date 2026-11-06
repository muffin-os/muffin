//! This module implements `pthread_mutex_*`, `pthread_mutexattr_*` and
//! [`Locked`]. [`Locked`] is the lock of the shared tables of the runtime.
//!
//! The uncontended lock and the uncontended unlock are one atomic operation
//! each. A thread that finds the mutex locked marks it contended. The thread
//! then parks in `SYS_ADDR_WAIT` on the state word. An unlock of a contended
//! mutex wakes one waiter.

use core::cell::UnsafeCell;
use core::ffi::c_int;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use kernel_abi::{EBUSY, EDEADLK, EINVAL, ENOTSUP, EPERM, Errno};
use libc::{
    PTHREAD_MUTEX_ERRORCHECK, PTHREAD_MUTEX_NORMAL, PTHREAD_MUTEX_RECURSIVE, pthread_mutex_t,
    pthread_mutexattr_t,
};

use super::syscall::{addr_wait, addr_wake};
use super::{NO_OWNER, owner_id};
use crate::errno::error_code;

/// Bits 0-1 of the state word hold the lock state.
const LOCK_STATE: u32 = 0b011;
const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
/// The mutex has a holder, and a thread can wait in `SYS_ADDR_WAIT` for the
/// unlock.
const CONTENDED: u32 = 2;
/// Bit 2 of the state word is 1 for an `ERRORCHECK` mutex.
const ERRORCHECK: u32 = 0b100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MutexKind {
    /// A relock by the owner blocks forever.
    Normal,
    /// A relock by the owner fails with `EDEADLK`. An unlock by another thread
    /// fails with `EPERM`.
    ErrorCheck,
}

impl MutexKind {
    fn of_state(state: u32) -> Self {
        if state & ERRORCHECK == 0 {
            Self::Normal
        } else {
            Self::ErrorCheck
        }
    }

    const fn state_bits(self) -> u32 {
        match self {
            Self::Normal => 0,
            Self::ErrorCheck => ERRORCHECK,
        }
    }
}

impl TryFrom<c_int> for MutexKind {
    type Error = Errno;

    fn try_from(kind: c_int) -> Result<Self, Errno> {
        match kind {
            PTHREAD_MUTEX_NORMAL => Ok(Self::Normal),
            PTHREAD_MUTEX_ERRORCHECK => Ok(Self::ErrorCheck),
            PTHREAD_MUTEX_RECURSIVE => Err(ENOTSUP),
            _ => Err(EINVAL),
        }
    }
}

/// This struct is the layout of `pthread_mutex_t`. All-zero bytes describe an
/// unlocked `NORMAL` mutex. This value is `PTHREAD_MUTEX_INITIALIZER`.
#[repr(C)]
pub(super) struct RawMutex {
    /// This field holds the lock state and the kind.
    state: AtomicU32,
    /// This field holds the low 32 bits of the owner tid. Only `ERRORCHECK`
    /// mutexes keep it.
    owner: AtomicU32,
}

const _: () = assert!(size_of::<RawMutex>() == size_of::<pthread_mutex_t>());
const _: () = assert!(align_of::<RawMutex>() <= align_of::<pthread_mutex_t>());

impl RawMutex {
    const fn new(kind: MutexKind) -> Self {
        Self {
            state: AtomicU32::new(UNLOCKED | kind.state_bits()),
            owner: AtomicU32::new(NO_OWNER),
        }
    }

    /// # Safety
    ///
    /// `raw` must point to a mutex with initialization from
    /// `pthread_mutex_init` or `PTHREAD_MUTEX_INITIALIZER`. The mutex must
    /// outlive `'a`.
    pub(super) unsafe fn from_raw<'a>(raw: *mut pthread_mutex_t) -> &'a Self {
        unsafe { &*raw.cast::<Self>() }
    }

    /// The kind bit never changes after the initialization.
    fn kind(&self) -> MutexKind {
        MutexKind::of_state(self.state.load(Relaxed))
    }

    pub(super) fn lock(&self) -> Result<(), Errno> {
        let kind = self.kind();
        // Only the owner writes its own id into `owner`, so a match means
        // this thread holds the mutex.
        if kind == MutexKind::ErrorCheck && self.owner.load(Relaxed) == owner_id() {
            return Err(EDEADLK);
        }
        self.acquire(kind.state_bits());
        self.take_ownership(kind);
        Ok(())
    }

    fn try_lock(&self) -> Result<(), Errno> {
        let kind = self.kind();
        let bits = kind.state_bits();
        self.state
            .compare_exchange(bits | UNLOCKED, bits | LOCKED, Acquire, Relaxed)
            .map_err(|_| EBUSY)?;
        self.take_ownership(kind);
        Ok(())
    }

    pub(super) fn unlock(&self) -> Result<(), Errno> {
        let kind = self.kind();
        if kind == MutexKind::ErrorCheck {
            if self.owner.load(Relaxed) != owner_id() {
                return Err(EPERM);
            }
            self.owner.store(NO_OWNER, Relaxed);
        }
        self.release(kind.state_bits());
        Ok(())
    }

    fn acquire(&self, kind_bits: u32) {
        if self
            .state
            .compare_exchange(kind_bits | UNLOCKED, kind_bits | LOCKED, Acquire, Relaxed)
            .is_err()
        {
            self.wait_until_acquired(kind_bits);
        }
    }

    /// This function marks the mutex contended and parks until this thread
    /// takes it. The mutex stays marked contended after the take, because
    /// other threads can still wait.
    fn wait_until_acquired(&self, kind_bits: u32) {
        let contended = kind_bits | CONTENDED;
        while self.state.swap(contended, Acquire) & LOCK_STATE != UNLOCKED {
            // `EAGAIN` and `EINTR` mean that the state changes or that a
            // signal interrupts the wait. The loop reads the state again in
            // both cases.
            let _ = addr_wait(&self.state, contended, None);
        }
    }

    fn release(&self, kind_bits: u32) {
        if self.state.swap(kind_bits | UNLOCKED, Release) & LOCK_STATE == CONTENDED {
            let woken = addr_wake(&self.state, 1);
            debug_assert!(woken.is_ok(), "SYS_ADDR_WAKE on a mutex failed: {woken:?}");
        }
    }

    fn take_ownership(&self, kind: MutexKind) {
        if kind == MutexKind::ErrorCheck {
            self.owner.store(owner_id(), Relaxed);
        }
    }

    fn is_locked(&self) -> bool {
        self.state.load(Relaxed) & LOCK_STATE != UNLOCKED
    }

    fn with_normal<R>(&self, access: impl FnOnce() -> R) -> R {
        let kind_bits = MutexKind::Normal.state_bits();
        self.acquire(kind_bits);
        let result = access();
        self.release(kind_bits);
        result
    }
}

/// This struct holds data of the runtime. One thread at a time accesses the
/// data, under a `NORMAL` mutex.
pub(crate) struct Locked<T> {
    mutex: RawMutex,
    data: UnsafeCell<T>,
}

// SAFETY: `with` gives out the data only while the caller holds the mutex.
unsafe impl<T: Send> Sync for Locked<T> {}

impl<T> Locked<T> {
    pub(crate) const fn new(data: T) -> Self {
        Self {
            mutex: RawMutex::new(MutexKind::Normal),
            data: UnsafeCell::new(data),
        }
    }

    /// This function runs `access` with the mutex held. `access` must not call
    /// `with` on the same value, because the relock blocks forever.
    pub(crate) fn with<R>(&self, access: impl FnOnce(&mut T) -> R) -> R {
        self.mutex.with_normal(|| {
            // SAFETY: the caller holds the mutex, so this is the only reference.
            access(unsafe { &mut *self.data.get() })
        })
    }
}

/// This struct is the layout of `pthread_mutexattr_t`. All-zero bytes describe
/// a `NORMAL` mutex.
#[repr(C)]
struct MutexAttr {
    kind: c_int,
}

const _: () = assert!(size_of::<MutexAttr>() == size_of::<pthread_mutexattr_t>());
const _: () = assert!(align_of::<MutexAttr>() <= align_of::<pthread_mutexattr_t>());

/// This function fails with `EINVAL` when `attr` holds no mutex type.
///
/// # Safety
///
/// `raw` must be valid for one `pthread_mutex_t` write, and `attr` must be
/// null or point to an attribute with initialization from
/// `pthread_mutexattr_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_init(
    raw: *mut pthread_mutex_t,
    attr: *const pthread_mutexattr_t,
) -> c_int {
    let kind = match unsafe { attr.cast::<MutexAttr>().as_ref() } {
        None => Ok(MutexKind::Normal),
        Some(attr) => MutexKind::try_from(attr.kind),
    };
    error_code(kind.map(|kind| unsafe { raw.cast::<RawMutex>().write(RawMutex::new(kind)) }))
}

/// This function fails with `EBUSY` on a locked mutex.
///
/// # Safety
///
/// `raw` must point to a mutex with initialization from `pthread_mutex_init`
/// or `PTHREAD_MUTEX_INITIALIZER`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_destroy(raw: *mut pthread_mutex_t) -> c_int {
    if unsafe { RawMutex::from_raw(raw) }.is_locked() {
        EBUSY.into()
    } else {
        0
    }
}

/// This function blocks forever on a relock by the owner of a `NORMAL` mutex.
/// It fails with `EDEADLK` on a relock by the owner of an `ERRORCHECK` mutex.
///
/// # Safety
///
/// The caller must satisfy the contract of [`pthread_mutex_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_lock(raw: *mut pthread_mutex_t) -> c_int {
    error_code(unsafe { RawMutex::from_raw(raw) }.lock())
}

/// This function fails with `EBUSY` on a locked mutex, also when the caller
/// holds it.
///
/// # Safety
///
/// The caller must satisfy the contract of [`pthread_mutex_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_trylock(raw: *mut pthread_mutex_t) -> c_int {
    error_code(unsafe { RawMutex::from_raw(raw) }.try_lock())
}

/// This function fails with `EPERM` when the caller does not hold an
/// `ERRORCHECK` mutex.
///
/// # Safety
///
/// The caller must satisfy the contract of [`pthread_mutex_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_unlock(raw: *mut pthread_mutex_t) -> c_int {
    error_code(unsafe { RawMutex::from_raw(raw) }.unlock())
}

/// # Safety
///
/// `attr` must be valid for one `pthread_mutexattr_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_init(attr: *mut pthread_mutexattr_t) -> c_int {
    let kind = PTHREAD_MUTEX_NORMAL;
    unsafe { attr.cast::<MutexAttr>().write(MutexAttr { kind }) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutexattr_destroy(_: *mut pthread_mutexattr_t) -> c_int {
    0
}

/// This function supports `PTHREAD_MUTEX_NORMAL` and
/// `PTHREAD_MUTEX_ERRORCHECK`. It fails with `ENOTSUP` for
/// `PTHREAD_MUTEX_RECURSIVE` and with `EINVAL` for other values.
///
/// # Safety
///
/// `attr` must point to an attribute with initialization from
/// `pthread_mutexattr_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_settype(
    attr: *mut pthread_mutexattr_t,
    kind: c_int,
) -> c_int {
    error_code(
        MutexKind::try_from(kind)
            .map(|_| unsafe { attr.cast::<MutexAttr>().write(MutexAttr { kind }) }),
    )
}
