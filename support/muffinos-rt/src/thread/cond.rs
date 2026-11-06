//! This module implements `pthread_cond_*` and `pthread_condattr_*` with a
//! sequence word.
//!
//! A waiter reads the sequence while it holds the mutex. It then unlocks the
//! mutex and parks in `SYS_ADDR_WAIT` while the sequence stays unchanged. A
//! signal or a broadcast changes the sequence before it wakes the waiter. A
//! wake between the read and the park makes the park fail with `EAGAIN`. The
//! wake therefore does not get lost.

use core::ffi::c_int;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::{Relaxed, Release};
use core::time::Duration;

use kernel_abi::{ClockId, ETIMEDOUT, Errno};
use libc::{
    CLOCK_REALTIME, clockid_t, pthread_cond_t, pthread_condattr_t, pthread_mutex_t, timespec,
};

use super::mutex::RawMutex;
use super::syscall::{addr_wait, addr_wake};
use crate::errno::error_code;
use crate::time::time_left;

/// This struct is the layout of `pthread_cond_t`. All-zero bytes describe a
/// condition variable on `CLOCK_REALTIME`. This value is
/// `PTHREAD_COND_INITIALIZER`.
#[repr(C)]
struct Cond {
    sequence: AtomicU32,
    /// This field holds the `ClockId` that `pthread_cond_timedwait` deadlines
    /// use.
    clock: u32,
}

const _: () = assert!(size_of::<Cond>() == size_of::<pthread_cond_t>());
const _: () = assert!(align_of::<Cond>() <= align_of::<pthread_cond_t>());

impl Cond {
    fn new(clock: ClockId) -> Self {
        Self {
            sequence: AtomicU32::new(0),
            clock: clock as u32,
        }
    }

    /// # Safety
    ///
    /// `raw` must point to a condition variable with initialization from
    /// `pthread_cond_init` or `PTHREAD_COND_INITIALIZER`. The condition
    /// variable must outlive `'a`.
    unsafe fn from_raw<'a>(raw: *mut pthread_cond_t) -> &'a Self {
        unsafe { &*raw.cast::<Self>() }
    }

    fn clock(&self) -> Result<ClockId, Errno> {
        ClockId::try_from(self.clock as usize)
    }

    /// This function unlocks `mutex`, waits for a signal, a broadcast or the
    /// end of `timeout`, and locks `mutex` again. It can return without a
    /// signal. This return is a spurious wakeup, which POSIX permits.
    fn wait(&self, mutex: &RawMutex, timeout: Option<Duration>) -> Result<(), Errno> {
        let sequence = self.sequence.load(Relaxed);
        mutex.unlock()?;
        let waited = addr_wait(&self.sequence, sequence, timeout);
        mutex.lock()?;
        match waited {
            Err(ETIMEDOUT) => Err(ETIMEDOUT),
            _ => Ok(()),
        }
    }

    fn wake(&self, count: usize) -> Result<(), Errno> {
        self.sequence.fetch_add(1, Release);
        addr_wake(&self.sequence, count).map(drop)
    }
}

/// This struct is the layout of `pthread_condattr_t`. All-zero bytes select
/// `CLOCK_REALTIME`.
#[repr(C)]
struct CondAttr {
    clock: clockid_t,
}

const _: () = assert!(size_of::<CondAttr>() == size_of::<pthread_condattr_t>());
const _: () = assert!(align_of::<CondAttr>() <= align_of::<pthread_condattr_t>());

/// # Safety
///
/// `cond` must be valid for one `pthread_cond_t` write, and `attr` must be
/// null or point to an attribute with initialization from
/// `pthread_condattr_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_init(
    cond: *mut pthread_cond_t,
    attr: *const pthread_condattr_t,
) -> c_int {
    let clock =
        unsafe { attr.cast::<CondAttr>().as_ref() }.map_or(CLOCK_REALTIME, |attr| attr.clock);
    error_code(
        ClockId::try_from(clock)
            .map(|clock| unsafe { cond.cast::<Cond>().write(Cond::new(clock)) }),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_cond_destroy(_: *mut pthread_cond_t) -> c_int {
    0
}

/// # Safety
///
/// `cond` must point to a condition variable with initialization from
/// `pthread_cond_init` or `PTHREAD_COND_INITIALIZER`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_signal(cond: *mut pthread_cond_t) -> c_int {
    error_code(unsafe { Cond::from_raw(cond) }.wake(1))
}

/// # Safety
///
/// The caller must satisfy the contract of [`pthread_cond_signal`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_broadcast(cond: *mut pthread_cond_t) -> c_int {
    error_code(unsafe { Cond::from_raw(cond) }.wake(usize::MAX))
}

/// # Safety
///
/// The caller must satisfy the contract of [`pthread_cond_signal`] for
/// `cond`. `mutex` must point to a mutex that the caller holds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_wait(
    cond: *mut pthread_cond_t,
    mutex: *mut pthread_mutex_t,
) -> c_int {
    let cond = unsafe { Cond::from_raw(cond) };
    error_code(cond.wait(unsafe { RawMutex::from_raw(mutex) }, None))
}

/// This function waits until `deadline` on the clock of `cond` at the latest.
/// It fails with `ETIMEDOUT` when the deadline passes.
/// This includes a deadline that precedes the call.
/// It fails with `EINVAL` for a deadline outside the POSIX `timespec` range.
///
/// # Safety
///
/// The caller must satisfy the contract of [`pthread_cond_wait`]. `deadline`
/// must be valid for one read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_timedwait(
    cond: *mut pthread_cond_t,
    mutex: *mut pthread_mutex_t,
    deadline: *const timespec,
) -> c_int {
    let cond = unsafe { Cond::from_raw(cond) };
    let mutex = unsafe { RawMutex::from_raw(mutex) };
    let deadline = unsafe { &*deadline };
    error_code(
        cond.clock()
            .and_then(|clock| time_left(clock, deadline))
            .and_then(|left| match left {
                Some(left) => cond.wait(mutex, Some(left)),
                None => Err(ETIMEDOUT),
            }),
    )
}

/// # Safety
///
/// `attr` must be valid for one `pthread_condattr_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_init(attr: *mut pthread_condattr_t) -> c_int {
    let clock = CLOCK_REALTIME;
    unsafe { attr.cast::<CondAttr>().write(CondAttr { clock }) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_condattr_destroy(_: *mut pthread_condattr_t) -> c_int {
    0
}

/// This function fails with `EINVAL` for a clock other than `CLOCK_REALTIME`
/// and `CLOCK_MONOTONIC`.
///
/// # Safety
///
/// `attr` must point to an attribute with initialization from
/// `pthread_condattr_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_setclock(
    attr: *mut pthread_condattr_t,
    clock: clockid_t,
) -> c_int {
    error_code(
        ClockId::try_from(clock)
            .map(|_| unsafe { attr.cast::<CondAttr>().write(CondAttr { clock }) }),
    )
}
