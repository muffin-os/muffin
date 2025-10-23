use core::ptr::{with_exposed_provenance, with_exposed_provenance_mut};

use kernel_abi::{EINVAL, Errno};
use thiserror::Error;

#[derive(Copy, Clone)]
pub struct UserspacePtr<T> {
    ptr: *const T,
}

#[derive(Debug, Error)]
#[error("not a userspace pointer: 0x{0:#x}")]
pub struct NotUserspace(usize);

impl From<NotUserspace> for Errno {
    fn from(_: NotUserspace) -> Self {
        EINVAL
    }
}

impl<T> TryFrom<*const T> for UserspacePtr<T> {
    type Error = NotUserspace;

    fn try_from(ptr: *const T) -> Result<Self, Self::Error> {
        unsafe {
            // Safety: we use a valid pointer
            Self::try_from_usize(ptr as usize)
        }
    }
}

impl<T> UserspacePtr<T> {
    /// # Safety
    /// The caller must ensure that the passed address is a valid pointer.
    /// It is explicitly safe to pass a pointer that is not in userspace.
    pub unsafe fn try_from_usize(ptr: usize) -> Result<Self, NotUserspace> {
        #[cfg(not(target_pointer_width = "64"))]
        compile_error!("only 64bit pointer width is supported");
        if ptr & 1 << 63 != 0 {
            Err(NotUserspace(ptr))
        } else {
            Ok(Self {
                ptr: with_exposed_provenance(ptr),
            })
        }
    }

    /// Validates that the pointer and size are within userspace bounds.
    /// 
    /// # Safety
    /// This function checks that ptr + size doesn't overflow into kernel space (upper half).
    pub unsafe fn validate_range(&self, size: usize) -> Result<(), NotUserspace> {
        let start = self.addr();
        let end = start.checked_add(size).ok_or(NotUserspace(start))?;
        
        // Check that the end address is still in lower half (bit 63 not set)
        if end & 1 << 63 != 0 {
            Err(NotUserspace(end))
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    pub fn as_ptr(&self) -> *const T {
        self.ptr
    }
}

pub struct UserspaceMutPtr<T> {
    ptr: *mut T,
}

impl<T> TryFrom<*mut T> for UserspaceMutPtr<T> {
    type Error = NotUserspace;

    fn try_from(ptr: *mut T) -> Result<Self, Self::Error> {
        unsafe {
            // Safety: we use a valid pointer
            Self::try_from_usize(ptr as usize)
        }
    }
}

impl<T> !Clone for UserspaceMutPtr<T> {}

impl<T> UserspaceMutPtr<T> {
    /// # Safety
    /// The caller must ensure that the passed address is a valid mutable pointer.
    /// It is explicitly safe to pass a pointer that is not in userspace.
    pub unsafe fn try_from_usize(ptr: usize) -> Result<Self, NotUserspace> {
        #[cfg(not(target_pointer_width = "64"))]
        compile_error!("only 64bit pointer width is supported");
        if ptr & 1 << 63 != 0 {
            Err(NotUserspace(ptr))
        } else {
            Ok(Self {
                ptr: with_exposed_provenance_mut(ptr),
            })
        }
    }

    #[must_use]
    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    pub fn as_ptr(&self) -> *const T {
        self.ptr as *const T
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.ptr
    }
}
