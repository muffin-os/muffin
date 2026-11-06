//! This module implements `pthread_key_*` with a global key table and a value
//! array for each thread.

use core::cell::Cell;
use core::ffi::{c_int, c_void};
use core::ptr;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::{EAGAIN, EINVAL, Errno};
use libc::{PTHREAD_DESTRUCTOR_ITERATIONS, PTHREAD_KEYS_MAX, pthread_key_t};

use super::mutex::Locked;
use crate::errno::error_code;

const KEY_COUNT: usize = PTHREAD_KEYS_MAX as usize;

type Destructor = unsafe extern "C" fn(*mut c_void);

/// A key exists while its generation is odd. `pthread_key_create` and
/// `pthread_key_delete` each add one to the generation. A value that a thread
/// stores under a deleted key therefore never shows under a later key in the
/// same slot.
fn exists(generation: usize) -> bool {
    generation % 2 == 1
}

struct KeyTable {
    /// This field holds the generation of each key. It changes only under
    /// `destructors`, so readers need no lock.
    generations: [AtomicUsize; KEY_COUNT],
    destructors: Locked<[Option<Destructor>; KEY_COUNT]>,
}

impl KeyTable {
    fn create(&self, destructor: Option<Destructor>) -> Result<pthread_key_t, Errno> {
        self.destructors.with(|destructors| {
            let key = self
                .generations
                .iter()
                .position(|generation| !exists(generation.load(Relaxed)))
                .ok_or(EAGAIN)?;
            destructors[key] = destructor;
            self.generations[key].fetch_add(1, Relaxed);
            Ok(key)
        })
    }

    fn delete(&self, key: pthread_key_t) -> Result<(), Errno> {
        let generation = self.generations.get(key).ok_or(EINVAL)?;
        self.destructors.with(|destructors| {
            if !exists(generation.load(Relaxed)) {
                return Err(EINVAL);
            }
            destructors[key] = None;
            generation.fetch_add(1, Relaxed);
            Ok(())
        })
    }

    /// This function returns the generation of `key`. It returns `None` when
    /// `key` does not exist.
    fn live_generation(&self, key: pthread_key_t) -> Option<usize> {
        let generation = self.generations.get(key)?.load(Relaxed);
        exists(generation).then_some(generation)
    }

    /// This function returns the destructor of `key` while `key` still has
    /// `generation`.
    fn destructor(&self, key: pthread_key_t, generation: usize) -> Option<Destructor> {
        self.destructors.with(|destructors| {
            (self.generations[key].load(Relaxed) == generation)
                .then_some(destructors[key])
                .flatten()
        })
    }
}

static KEYS: KeyTable = KeyTable {
    generations: [const { AtomicUsize::new(0) }; KEY_COUNT],
    destructors: Locked::new([None; KEY_COUNT]),
};

/// This struct holds the value of one key in one thread and the key
/// generation of the value.
#[derive(Clone, Copy)]
struct Value {
    generation: usize,
    data: *mut c_void,
}

impl Value {
    const NULL: Self = Self {
        generation: 0,
        data: ptr::null_mut(),
    };
}

#[thread_local]
static VALUES: [Cell<Value>; KEY_COUNT] = [const { Cell::new(Value::NULL) }; KEY_COUNT];

/// This function runs the destructors of the non-null values of the calling
/// thread. It runs rounds while a destructor stores a new value. It runs at
/// most `PTHREAD_DESTRUCTOR_ITERATIONS` rounds.
pub(super) fn run_destructors() {
    for _ in 0..PTHREAD_DESTRUCTOR_ITERATIONS {
        let mut ran = false;
        for (key, slot) in VALUES.iter().enumerate() {
            let value = slot.replace(Value::NULL);
            if value.data.is_null() {
                continue;
            }
            if let Some(destructor) = KEYS.destructor(key, value.generation) {
                unsafe { destructor(value.data) };
                ran = true;
            }
        }
        if !ran {
            return;
        }
    }
}

/// This function fails with `EAGAIN` when all `PTHREAD_KEYS_MAX` keys exist.
///
/// # Safety
///
/// `key` must be valid for one `pthread_key_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_key_create(
    key: *mut pthread_key_t,
    destructor: Option<Destructor>,
) -> c_int {
    error_code(
        KEYS.create(destructor)
            .map(|created| unsafe { key.write(created) }),
    )
}

/// This function never runs a destructor. It drops the values of `key` in
/// other threads without one.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_key_delete(key: pthread_key_t) -> c_int {
    error_code(KEYS.delete(key))
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_getspecific(key: pthread_key_t) -> *mut c_void {
    match (VALUES.get(key), KEYS.live_generation(key)) {
        (Some(slot), Some(generation)) if slot.get().generation == generation => slot.get().data,
        _ => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_setspecific(key: pthread_key_t, data: *const c_void) -> c_int {
    let (Some(slot), Some(generation)) = (VALUES.get(key), KEYS.live_generation(key)) else {
        return EINVAL.into();
    };
    slot.set(Value {
        generation,
        data: data.cast_mut(),
    });
    0
}
