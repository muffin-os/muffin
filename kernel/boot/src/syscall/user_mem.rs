//! This module is the only kernel API that touches user memory.
//!
//! Kernel code never holds a pointer into user memory across a call that can
//! block. The crate `kernel_syscall` never dereferences a user pointer. The
//! dispatchers in `crate::syscall` copy data at the edges. They copy the user
//! arguments into kernel buffers before the generic logic runs. They copy the
//! results out after the logic returns.
//!
//! Each copy pins the user mappings of the process with a
//! [`UserMemoryGuard`]. While a guard exists, the process cannot remove,
//! split or re-protect any of its regions. Therefore a page that the copy
//! makes resident stays accessible until the copy ends. A guard holder never
//! parks. A writer takes [`Process::exclusive_user_memory`]. The writers are
//! `munmap`, `mprotect`, fork, exec and the release of the per-thread blocks.

use alloc::vec;
use alloc::vec::Vec;
use core::slice;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::Relaxed;

use kernel_abi::{EFAULT, EIO, ENOMEM, Errno};
use kernel_syscall::UserspacePtr;
use spin::{RwLockReadGuard, RwLockWriteGuard};
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;

use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::process::mem::PageInError;
use crate::mcore::mtask::task::Task;

/// This constant is the maximum number of bytes that one `read`, `write` or
/// `getdents` call copies. A larger request gets a short count, as POSIX
/// permits.
pub const MAX_USER_COPY_BYTES: usize = 1 << 20;

/// This type pins the user mappings of the current process. While a guard
/// exists, the process cannot remove, split or re-protect any region.
#[must_use]
pub struct UserMemoryGuard<'p> {
    _lock: RwLockReadGuard<'p, ()>,
    pins: &'static AtomicU32,
}

impl<'p> UserMemoryGuard<'p> {
    fn new(lock: RwLockReadGuard<'p, ()>) -> Self {
        let pins = Task::current().user_memory_pins();
        pins.fetch_add(1, Relaxed);
        Self { _lock: lock, pins }
    }
}

impl Drop for UserMemoryGuard<'_> {
    fn drop(&mut self) {
        self.pins.fetch_sub(1, Relaxed);
    }
}

impl Process {
    /// This function pins the user mappings of this process, which is the
    /// process of the current task. A reader never parks while it holds the
    /// guard, and `park_current_task` asserts this. The caller must keep
    /// interrupts on, because a writer waits for a TLB shootdown that this
    /// CPU must acknowledge.
    pub fn pin_user_memory(&self) -> UserMemoryGuard<'_> {
        debug_assert!(interrupts::are_enabled());
        UserMemoryGuard::new(self.user_memory.read())
    }

    /// This function does the same as [`Process::pin_user_memory`], for a
    /// context with interrupts off. It returns `None` when a writer holds
    /// the lock. The caller tries again later.
    pub fn try_pin_user_memory(&self) -> Option<UserMemoryGuard<'_>> {
        self.user_memory.try_read().map(UserMemoryGuard::new)
    }

    /// This function excludes every user memory guard of this process. A
    /// writer that removes, splits or re-protects regions uses it.
    ///
    /// # Panics
    /// In debug builds, this function panics when interrupts are off. It also
    /// panics when the current task holds a guard, because the wait would
    /// never end.
    pub fn exclusive_user_memory(&self) -> RwLockWriteGuard<'_, ()> {
        debug_assert!(interrupts::are_enabled());
        debug_assert_eq!(Task::current().user_memory_pins().load(Relaxed), 0);
        self.user_memory.write()
    }
}

/// This type is the access that a copy needs from each page of its range.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum UserAccess {
    Read,
    Write,
}

/// This function makes `[ptr, ptr + len)` resident, then checks the range
/// against `access`.
///
/// It leaves a page that no memory region backs unmapped and reports it. It
/// never pages in such a page.
///
/// # Errors
/// This function returns `EFAULT` for a non-canonical address and for a page
/// that cannot be mapped. It also returns `EFAULT` for a range that is
/// resident but not reachable with `access`. It returns `ENOMEM` when no
/// physical frame is available. It returns `EIO` when the backing file read
/// fails.
fn make_user_range_resident(
    process: &Process,
    ptr: usize,
    len: usize,
    access: UserAccess,
) -> Result<(), Errno> {
    let Ok(addr) = VirtAddr::try_new(ptr as u64) else {
        return Err(EFAULT);
    };

    let address_space = process.address_space();
    process
        .memory_regions()
        .populate(address_space, addr, len, access == UserAccess::Write)
        .map_err(|e| match e {
            PageInError::OutOfMemory => ENOMEM,
            PageInError::MapFailed | PageInError::NotWritable | PageInError::NotAccessible => {
                EFAULT
            }
            PageInError::ReadFailed => EIO,
        })?;

    let accessible = match access {
        UserAccess::Read => address_space.is_user_readable(addr, len),
        UserAccess::Write => address_space.is_user_writable(addr, len),
    };
    if accessible { Ok(()) } else { Err(EFAULT) }
}

/// This function checks only the address of `[ptr, ptr + len)` and makes no
/// page resident. A dispatcher calls it before work that can end with an
/// empty copy, which examines no address.
///
/// # Errors
/// This function returns `EFAULT` for a null `ptr`, and for a range that
/// overflows, is not canonical or leaves the lower half.
pub fn check_user_range(ptr: usize, len: usize) -> Result<(), Errno> {
    if ptr == 0 {
        return Err(EFAULT);
    }
    UserspacePtr::<u8>::try_from_usize(ptr)
        .and_then(|start| start.validate_range(len))
        .map_err(|_| EFAULT)
}

/// This function runs `f` on the user bytes `[ptr, ptr + len)` under a
/// [`UserMemoryGuard`]. `f` must not block. With [`UserAccess::Read`], `f`
/// must not write. A `len` of zero gives `f` an empty slice without a check
/// of `ptr`.
///
/// # Errors
/// This function returns `EFAULT` when the range is not lower-half memory
/// with `access`. It also returns each error of [`make_user_range_resident`].
pub fn with_user_slice<R>(
    ptr: usize,
    len: usize,
    access: UserAccess,
    f: impl FnOnce(&mut [u8]) -> R,
) -> Result<R, Errno> {
    if len == 0 {
        return Ok(f(&mut []));
    }
    check_user_range(ptr, len)?;

    let process = Process::current();
    let _pin = process.pin_user_memory();
    make_user_range_resident(process, ptr, len, access)?;
    // Safety: the range is lower half, resident and reachable with `access`.
    // The guard keeps it so until `f` returns.
    let bytes = unsafe { slice::from_raw_parts_mut(ptr as *mut u8, len) };
    Ok(f(bytes))
}

/// This function copies `len` user bytes from `ptr` into a kernel buffer.
///
/// # Errors
/// This function returns each error of [`with_user_slice`].
pub fn copy_in(ptr: usize, len: usize) -> Result<Vec<u8>, Errno> {
    with_user_slice(ptr, len, UserAccess::Read, |src| src.to_vec())
}

/// This function copies `src` to the user bytes at `ptr`.
///
/// # Errors
/// This function returns each error of [`with_user_slice`].
pub fn copy_out(ptr: usize, src: &[u8]) -> Result<(), Errno> {
    with_user_slice(ptr, src.len(), UserAccess::Write, |dst| {
        dst.copy_from_slice(src);
    })
}

/// This function reads a `T` from the user address `addr`. The caller picks a
/// `T` that is valid for every bit pattern.
///
/// # Errors
/// This function returns each error of [`with_user_slice`].
pub fn read_user<T: Copy>(addr: usize) -> Result<T, Errno> {
    with_user_slice(addr, size_of::<T>(), UserAccess::Read, |src| {
        // Safety: `src` holds the `size_of::<T>()` bytes of one `T`.
        unsafe { src.as_ptr().cast::<T>().read_unaligned() }
    })
}

/// This function writes `value` to the user address `addr`.
///
/// # Errors
/// This function returns each error of [`with_user_slice`].
pub fn write_user<T: Copy>(addr: usize, value: T) -> Result<(), Errno> {
    write_user_array(addr, &[value])
}

/// This function reads `count` consecutive `T` values from the user address
/// `addr`. The caller picks a `T` that is valid for every bit pattern.
///
/// # Errors
/// This function returns `EFAULT` when the byte size overflows. It also
/// returns each error of [`with_user_slice`].
pub fn read_user_array<T: Copy>(addr: usize, count: usize) -> Result<Vec<T>, Errno> {
    let byte_len = count.checked_mul(size_of::<T>()).ok_or(EFAULT)?;
    with_user_slice(addr, byte_len, UserAccess::Read, |src| {
        (0..count)
            .map(|i| {
                // Safety: `src` holds `count` values of `T`.
                unsafe { src.as_ptr().cast::<T>().add(i).read_unaligned() }
            })
            .collect()
    })
}

/// This function writes `values` to consecutive user addresses from `addr` on.
///
/// # Errors
/// This function returns each error of [`with_user_slice`].
pub fn write_user_array<T: Copy>(addr: usize, values: &[T]) -> Result<(), Errno> {
    with_user_slice(addr, size_of_val(values), UserAccess::Write, |dst| {
        for (i, value) in values.iter().enumerate() {
            // Safety: `dst` has room for `values.len()` values of `T`.
            unsafe { dst.as_mut_ptr().cast::<T>().add(i).write_unaligned(*value) };
        }
    })
}

/// This function returns a zeroed kernel buffer of `len` bytes, at most
/// [`MAX_USER_COPY_BYTES`].
pub fn bounce_buffer(len: usize) -> Vec<u8> {
    vec![0u8; len.min(MAX_USER_COPY_BYTES)]
}
