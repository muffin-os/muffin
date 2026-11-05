use core::fmt;
use core::ops::{Deref, DerefMut};

use spin::{Mutex, MutexGuard};
use x86_64::instructions::interrupts;

/// A spin lock that keeps interrupts off while a guard is alive.
///
/// An interrupt handler on the holding CPU cannot take this lock, so the
/// handler cannot deadlock on it.
pub struct IrqMutex<T: ?Sized> {
    inner: Mutex<T>,
}

impl<T> IrqMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }
}

impl<T: ?Sized> IrqMutex<T> {
    /// Turns interrupts off, then spins until the lock is free.
    ///
    /// Interrupts stay off until the guard drops. The guard restores the
    /// interrupt state from before this call.
    pub fn lock_without_interrupts(&self) -> IrqMutexGuard<'_, T> {
        let were_enabled = interrupts::are_enabled();
        interrupts::disable();
        IrqMutexGuard {
            data: self.inner.lock(),
            _interrupts: InterruptRestore { were_enabled },
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for IrqMutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IrqMutex")
            .field("inner", &self.inner)
            .finish()
    }
}

/// Grants access to the data of an [`IrqMutex`] while interrupts are off.
///
/// The guard must stay on the CPU that turned interrupts off, so it is `!Send`.
pub struct IrqMutexGuard<'a, T: ?Sized> {
    // The field order is the drop order. The guard must release the lock
    // before it turns interrupts on.
    data: MutexGuard<'a, T>,
    _interrupts: InterruptRestore,
}

impl<T: ?Sized> !Send for IrqMutexGuard<'_, T> {}

impl<T: ?Sized> Deref for IrqMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T: ?Sized> DerefMut for IrqMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.data
    }
}

struct InterruptRestore {
    were_enabled: bool,
}

impl Drop for InterruptRestore {
    fn drop(&mut self) {
        if self.were_enabled {
            interrupts::enable();
        }
    }
}
