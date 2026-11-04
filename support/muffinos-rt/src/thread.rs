//! pthread functions for the single thread of a muffin process.
//!
//! No second thread exists, so a lock that is held can never be released
//! by another thread, and a condition variable can never be signalled.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_void};
use core::ptr;

use kernel_abi::{ClockId, EAGAIN, EBUSY, EDEADLK, EINVAL, EPERM, ERANGE, ESRCH, ETIMEDOUT};
use libc::{
    CLOCK_REALTIME, PTHREAD_STACK_MIN, clockid_t, pthread_attr_t, pthread_cond_t,
    pthread_condattr_t, pthread_key_t, pthread_mutex_t, pthread_mutexattr_t, pthread_t, timespec,
};

use crate::time::sleep_until;

/// Thread id `pthread_self` returns for the only thread.
const MAIN_THREAD: pthread_t = 1;

/// State this crate keeps inside a `pthread_mutex_t`. All zeros, the value
/// of `PTHREAD_MUTEX_INITIALIZER`, is an unlocked mutex.
#[repr(C)]
struct Mutex {
    is_locked: bool,
}

const _: () = assert!(size_of::<Mutex>() <= size_of::<pthread_mutex_t>());

/// # Safety
///
/// `mutex` must point to a live, initialized `pthread_mutex_t`.
unsafe fn mutex<'a>(mutex: *mut pthread_mutex_t) -> &'a mut Mutex {
    unsafe { &mut *mutex.cast::<Mutex>() }
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_self() -> pthread_t {
    MAIN_THREAD
}

/// # Safety
///
/// `raw` must be valid for one `pthread_mutex_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_init(
    raw: *mut pthread_mutex_t,
    _: *const pthread_mutexattr_t,
) -> c_int {
    unsafe { raw.cast::<Mutex>().write(Mutex { is_locked: false }) };
    0
}

/// # Safety
///
/// `raw` must point to a mutex that `pthread_mutex_init` or
/// `PTHREAD_MUTEX_INITIALIZER` set up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_destroy(raw: *mut pthread_mutex_t) -> c_int {
    if unsafe { mutex(raw) }.is_locked {
        EBUSY.into()
    } else {
        0
    }
}

/// Fails with `EDEADLK` on a locked mutex, because only this thread can hold it.
///
/// # Safety
///
/// Same contract as [`pthread_mutex_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_lock(raw: *mut pthread_mutex_t) -> c_int {
    let mutex = unsafe { mutex(raw) };
    if mutex.is_locked {
        return EDEADLK.into();
    }
    mutex.is_locked = true;
    0
}

/// # Safety
///
/// Same contract as [`pthread_mutex_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_trylock(raw: *mut pthread_mutex_t) -> c_int {
    let mutex = unsafe { mutex(raw) };
    if mutex.is_locked {
        return EBUSY.into();
    }
    mutex.is_locked = true;
    0
}

/// # Safety
///
/// Same contract as [`pthread_mutex_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_unlock(raw: *mut pthread_mutex_t) -> c_int {
    let mutex = unsafe { mutex(raw) };
    if !mutex.is_locked {
        return EPERM.into();
    }
    mutex.is_locked = false;
    0
}

/// The mutex type has no effect: a relock fails with `EDEADLK` for every type.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutexattr_init(_: *mut pthread_mutexattr_t) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutexattr_destroy(_: *mut pthread_mutexattr_t) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutexattr_settype(_: *mut pthread_mutexattr_t, _: c_int) -> c_int {
    0
}

/// Clock a condition variable measures `pthread_cond_timedwait` deadlines on.
/// `pthread_cond_t` and `pthread_condattr_t` both hold one. All zeros, the
/// value of `PTHREAD_COND_INITIALIZER`, is `CLOCK_REALTIME`.
#[repr(C)]
struct CondClock {
    clock: clockid_t,
}

const _: () = assert!(size_of::<CondClock>() <= size_of::<pthread_cond_t>());
const _: () = assert!(size_of::<CondClock>() <= size_of::<pthread_condattr_t>());

/// # Safety
///
/// `cond` must be valid for one `pthread_cond_t` write, and `attr` must be
/// null or point to an attribute that `pthread_condattr_init` set up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_init(
    cond: *mut pthread_cond_t,
    attr: *const pthread_condattr_t,
) -> c_int {
    let clock = if attr.is_null() {
        CLOCK_REALTIME
    } else {
        unsafe { (*attr.cast::<CondClock>()).clock }
    };
    unsafe { cond.cast::<CondClock>().write(CondClock { clock }) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_cond_destroy(_: *mut pthread_cond_t) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_cond_signal(_: *mut pthread_cond_t) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_cond_broadcast(_: *mut pthread_cond_t) -> c_int {
    0
}

/// Fails with `EDEADLK`, because no other thread can signal the condition.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_cond_wait(_: *mut pthread_cond_t, _: *mut pthread_mutex_t) -> c_int {
    EDEADLK.into()
}

/// Sleeps until `deadline` on the clock of `cond` and returns `ETIMEDOUT`,
/// because no other thread can signal the condition. Fails with `EINVAL`
/// for a deadline outside the POSIX `timespec` range.
///
/// # Safety
///
/// `cond` must point to a condition that `pthread_cond_init` or
/// `PTHREAD_COND_INITIALIZER` set up, and `deadline` must be valid for one read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_timedwait(
    cond: *mut pthread_cond_t,
    _: *mut pthread_mutex_t,
    deadline: *const timespec,
) -> c_int {
    let clock = unsafe { (*cond.cast::<CondClock>()).clock };
    let slept =
        ClockId::try_from(clock).and_then(|clock| sleep_until(clock, unsafe { &*deadline }));
    match slept {
        Ok(()) => ETIMEDOUT.into(),
        Err(errno) => errno.into(),
    }
}

/// # Safety
///
/// `attr` must be valid for one `pthread_condattr_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_init(attr: *mut pthread_condattr_t) -> c_int {
    let clock = CLOCK_REALTIME;
    unsafe { attr.cast::<CondClock>().write(CondClock { clock }) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_condattr_destroy(_: *mut pthread_condattr_t) -> c_int {
    0
}

/// # Safety
///
/// `attr` must point to an attribute that `pthread_condattr_init` set up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_setclock(
    attr: *mut pthread_condattr_t,
    clock: clockid_t,
) -> c_int {
    if let Err(errno) = ClockId::try_from(clock) {
        return errno.into();
    }
    unsafe { attr.cast::<CondClock>().write(CondClock { clock }) };
    0
}

const KEY_COUNT: usize = 128;

/// Value of each thread-specific data key. `None` marks a free key.
struct Keys(UnsafeCell<[Option<*mut c_void>; KEY_COUNT]>);

// SAFETY: the process has one thread, so no two accesses overlap.
unsafe impl Sync for Keys {}

impl Keys {
    fn with<R>(&self, access: impl FnOnce(&mut [Option<*mut c_void>; KEY_COUNT]) -> R) -> R {
        // SAFETY: the process has one thread, and no `access` closure calls
        // `with` again, so this is the only reference.
        access(unsafe { &mut *self.0.get() })
    }
}

static KEYS: Keys = Keys(UnsafeCell::new([None; KEY_COUNT]));

/// Never runs `destructor`. Destructors run when a thread exits, and the only
/// thread exits only with the process.
///
/// # Safety
///
/// `key` must be valid for one `pthread_key_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_key_create(
    key: *mut pthread_key_t,
    _destructor: Option<unsafe extern "C" fn(*mut c_void)>,
) -> c_int {
    let created = KEYS.with(|keys| {
        let free = keys.iter().position(Option::is_none)?;
        keys[free] = Some(ptr::null_mut());
        Some(free)
    });
    let Some(created) = created else {
        return EAGAIN.into();
    };
    unsafe { key.write(created) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_key_delete(key: pthread_key_t) -> c_int {
    KEYS.with(|keys| match keys.get_mut(key) {
        Some(slot @ Some(_)) => {
            *slot = None;
            0
        }
        _ => EINVAL.into(),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_getspecific(key: pthread_key_t) -> *mut c_void {
    KEYS.with(|keys| keys.get(key).copied().flatten().unwrap_or(ptr::null_mut()))
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_setspecific(key: pthread_key_t, value: *const c_void) -> c_int {
    KEYS.with(|keys| match keys.get_mut(key) {
        Some(Some(stored)) => {
            *stored = value.cast_mut();
            0
        }
        _ => EINVAL.into(),
    })
}

/// Fails with `EAGAIN`, because the kernel has no thread syscall.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_create(
    _: *mut pthread_t,
    _: *const pthread_attr_t,
    _: extern "C" fn(*mut c_void) -> *mut c_void,
    _: *mut c_void,
) -> c_int {
    EAGAIN.into()
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_join(_: pthread_t, _: *mut *mut c_void) -> c_int {
    ESRCH.into()
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_detach(_: pthread_t) -> c_int {
    ESRCH.into()
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_init(_: *mut pthread_attr_t) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_destroy(_: *mut pthread_attr_t) -> c_int {
    0
}

/// Ignores the size, because no thread is ever created from an attribute.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_setstacksize(_: *mut pthread_attr_t, _: usize) -> c_int {
    0
}

/// # Safety
///
/// `size` must be valid for one `usize` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_attr_getstacksize(
    _: *const pthread_attr_t,
    size: *mut usize,
) -> c_int {
    unsafe { size.write(PTHREAD_STACK_MIN) };
    0
}

/// # Safety
///
/// `size` must be valid for one `usize` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_attr_getguardsize(
    _: *const pthread_attr_t,
    size: *mut usize,
) -> c_int {
    unsafe { size.write(0) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_setname_np(_: pthread_t, _: *const c_char) -> c_int {
    0
}

/// Writes the empty name, because `pthread_setname_np` stores no name.
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
