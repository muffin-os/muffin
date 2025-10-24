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
        if is_upper_half(ptr) {
            Err(NotUserspace(ptr))
        } else {
            Ok(Self {
                ptr: with_exposed_provenance(ptr),
            })
        }
    }

    /// Validates that the pointer and size are within userspace bounds.
    /// 
    /// This function checks that ptr + size doesn't overflow into kernel space (upper half).
    pub fn validate_range(&self, size: usize) -> Result<(), NotUserspace> {
        let start = self.addr();
        let end = start.checked_add(size).ok_or(NotUserspace(start))?;
        
        if is_upper_half(end) {
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

/// Checks if an address is in the upper half (kernel space).
/// Upper half addresses have bit 63 set.
#[inline]
fn is_upper_half(addr: usize) -> bool {
    addr & 1 << 63 != 0
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_range_valid_small() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0x1000).unwrap() };
        assert!(ptr.validate_range(4096).is_ok());
    }

    #[test]
    fn test_validate_range_valid_large() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0x1000_0000).unwrap() };
        assert!(ptr.validate_range(0x1000_0000).is_ok());
    }

    #[test]
    fn test_validate_range_zero_size() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0x1000).unwrap() };
        assert!(ptr.validate_range(0).is_ok());
    }

    #[test]
    fn test_validate_range_at_boundary() {
        // Maximum valid lower-half address
        let max_lower_half = (1_usize << 63) - 1;
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(max_lower_half).unwrap() };
        // Size 0 should be OK (no overflow)
        assert!(ptr.validate_range(0).is_ok());
        // Size 1 would overflow into upper half
        assert!(ptr.validate_range(1).is_err());
    }

    #[test]
    fn test_validate_range_overflow_into_upper_half() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0x7FFF_FFFF_FFFF_F000).unwrap() };
        // This would overflow into the upper half (kernel space)
        assert!(ptr.validate_range(0x2000).is_err());
    }

    #[test]
    fn test_validate_range_arithmetic_overflow() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0x7FFF_FFFF_FFFF_FFFF).unwrap() };
        // This would cause usize overflow
        assert!(ptr.validate_range(usize::MAX).is_err());
    }

    #[test]
    fn test_validate_range_near_boundary() {
        // Test various sizes near the boundary
        // Upper half starts at 0x8000_0000_0000_0000
        let base = 0x7FFF_FFFF_FFFF_F000_usize;
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(base).unwrap() };
        
        // Should be OK: base + 0xFFF = 0x7FFF_FFFF_FFFF_FFFF (max lower half)
        assert!(ptr.validate_range(0xFFF).is_ok());
        // Should fail: base + 0x1000 = 0x8000_0000_0000_0000 (bit 63 set)
        assert!(ptr.validate_range(0x1000).is_err());
        // Should also fail: anything larger
        assert!(ptr.validate_range(0x2000).is_err());
    }

    #[test]
    fn test_validate_range_from_zero() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0).unwrap() };
        // Can map up to the entire lower half
        let max_lower_half = (1_usize << 63) - 1;
        assert!(ptr.validate_range(max_lower_half).is_ok());
        // But not including the boundary
        assert!(ptr.validate_range(1_usize << 63).is_err());
    }

    #[test]
    fn test_validate_range_max_size() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(1).unwrap() };
        // Maximum possible size without overflow
        assert!(ptr.validate_range(usize::MAX - 1).is_err());
    }
}
