use core::fmt::{Debug, Formatter};
use core::ptr::{with_exposed_provenance, with_exposed_provenance_mut};

use kernel_abi::{EINVAL, Errno};
use thiserror::Error;
use x86_64::VirtAddr;

#[derive(Copy, Clone)]
pub struct UserspacePtr<T> {
    ptr: *const T,
}

impl<T> Debug for UserspacePtr<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.addr())
    }
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
        Self::try_from_usize(ptr as usize)
    }
}

impl<T> UserspacePtr<T> {
    /// # Errors
    /// Returns [`NotUserspace`] if `ptr` is non-canonical or in the upper half.
    pub fn try_from_usize(ptr: usize) -> Result<Self, NotUserspace> {
        #[cfg(not(target_pointer_width = "64"))]
        compile_error!("only 64bit pointer width is supported");

        if is_upper_half(ptr)? {
            Err(NotUserspace(ptr))
        } else {
            Ok(Self {
                ptr: with_exposed_provenance(ptr),
            })
        }
    }

    /// Checks that `size` bytes from this pointer stay in the lower half.
    ///
    /// A range may end exactly at the top of the lower half. An empty range
    /// is always valid because the pointer itself was already checked.
    pub fn validate_range(&self, size: usize) -> Result<(), NotUserspace> {
        validate_range(self.addr(), size)
    }

    #[must_use]
    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    pub fn as_ptr(&self) -> *const T {
        self.ptr
    }
}

const UPPER_HALF_START: u64 = 0xFFFF_8000_0000_0000;

// Errors on a non-canonical address.
#[inline]
fn is_upper_half(addr: usize) -> Result<bool, NotUserspace> {
    let virt_addr = VirtAddr::try_new(addr as u64).map_err(|_| NotUserspace(addr))?;
    Ok(virt_addr.as_u64() >= UPPER_HALF_START)
}

fn validate_range(start: usize, size: usize) -> Result<(), NotUserspace> {
    let Some(len_to_last) = size.checked_sub(1) else {
        return Ok(());
    };
    let last = start.checked_add(len_to_last).ok_or(NotUserspace(start))?;
    if is_upper_half(last)? {
        Err(NotUserspace(last))
    } else {
        Ok(())
    }
}

pub struct UserspaceMutPtr<T> {
    ptr: *mut T,
}

impl<T> Debug for UserspaceMutPtr<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.ptr as usize)
    }
}

impl<T> TryFrom<*mut T> for UserspaceMutPtr<T> {
    type Error = NotUserspace;

    fn try_from(ptr: *mut T) -> Result<Self, Self::Error> {
        Self::try_from_usize(ptr as usize)
    }
}

impl<T> !Clone for UserspaceMutPtr<T> {}

impl<T> UserspaceMutPtr<T> {
    /// # Errors
    /// Returns [`NotUserspace`] if `ptr` is non-canonical or in the upper half.
    pub fn try_from_usize(ptr: usize) -> Result<Self, NotUserspace> {
        #[cfg(not(target_pointer_width = "64"))]
        compile_error!("only 64bit pointer width is supported");
        if is_upper_half(ptr)? {
            Err(NotUserspace(ptr))
        } else {
            Ok(Self {
                ptr: with_exposed_provenance_mut(ptr),
            })
        }
    }

    /// Checks that `size` bytes from this pointer stay in the lower half.
    ///
    /// A range may end exactly at the top of the lower half. An empty range
    /// is always valid because the pointer itself was already checked.
    pub fn validate_range(&self, size: usize) -> Result<(), NotUserspace> {
        validate_range(self.addr(), size)
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
        let ptr = UserspacePtr::<u8>::try_from_usize(0x1000).unwrap();
        assert!(ptr.validate_range(4096).is_ok());
    }

    #[test]
    fn test_validate_range_valid_large() {
        let ptr = UserspacePtr::<u8>::try_from_usize(0x1000_0000).unwrap();
        assert!(ptr.validate_range(0x1000_0000).is_ok());
    }

    #[test]
    fn test_validate_range_zero_size() {
        let ptr = UserspacePtr::<u8>::try_from_usize(0x1000).unwrap();
        assert!(ptr.validate_range(0).is_ok());
    }

    #[test]
    fn test_validate_range_at_boundary() {
        // 0x0000_7FFF_FFFF_FFFF, the last lower-half address
        let max_lower_half = (1_usize << 47) - 1;
        let ptr = UserspacePtr::<u8>::try_from_usize(max_lower_half).unwrap();
        assert!(ptr.validate_range(0).is_ok());
        assert!(ptr.validate_range(1).is_ok());
        // The last byte 0x0000_8000_0000_0000 is non-canonical.
        assert!(ptr.validate_range(2).is_err());
    }

    #[test]
    fn test_validate_range_overflow_into_upper_half() {
        let ptr = UserspacePtr::<u8>::try_from_usize(0x0000_7FFF_FFFF_F000).unwrap();
        // The end address 0x0000_8000_0000_1000 is non-canonical.
        assert!(ptr.validate_range(0x2000).is_err());
    }

    #[test]
    fn test_validate_range_arithmetic_overflow() {
        let ptr = UserspacePtr::<u8>::try_from_usize(0x0000_7FFF_FFFF_FFFF).unwrap();
        assert!(ptr.validate_range(usize::MAX).is_err());
    }

    #[test]
    fn test_validate_range_near_boundary() {
        // The lower half ends at 0x0000_7FFF_FFFF_FFFF.
        let base = 0x0000_7FFF_FFFF_F000_usize;
        let ptr = UserspacePtr::<u8>::try_from_usize(base).unwrap();

        // The last page of the lower half is fully usable.
        assert!(ptr.validate_range(0x1000).is_ok());
        // base + 0x1000 = 0x0000_8000_0000_0000 is non-canonical
        assert!(ptr.validate_range(0x1001).is_err());
        assert!(ptr.validate_range(0x2000).is_err());
    }

    #[test]
    fn test_validate_range_from_zero() {
        let ptr = UserspacePtr::<u8>::try_from_usize(0).unwrap();
        // The whole lower half (47 bits) is addressable
        assert!(ptr.validate_range(1_usize << 47).is_ok());
        // One byte more reaches the non-canonical hole
        assert!(ptr.validate_range((1_usize << 47) + 1).is_err());
    }

    #[test]
    fn test_validate_range_max_size() {
        let ptr = UserspacePtr::<u8>::try_from_usize(1).unwrap();
        // Maximum possible size without overflow
        assert!(ptr.validate_range(usize::MAX - 1).is_err());
    }

    #[test]
    fn test_canonical_address_lower_half_max() {
        // Maximum canonical lower-half address (bit 47 = 0)
        let max_canonical_lower = 0x0000_7FFF_FFFF_FFFF_usize;
        let result = UserspacePtr::<u8>::try_from_usize(max_canonical_lower);
        assert!(result.is_ok());
    }

    #[test]
    fn test_canonical_address_upper_half_start() {
        // Minimum canonical upper-half address (bit 47 = 1, bits 48-63 must be 1)
        // This is 0xFFFF_8000_0000_0000, but we should reject it as it's kernel space
        let min_canonical_upper = 0xFFFF_8000_0000_0000_usize;
        let result = UserspacePtr::<u8>::try_from_usize(min_canonical_upper);
        assert!(result.is_err());
    }

    #[test]
    fn test_non_canonical_address_rejected() {
        // bit 47 set while bits 48-63 are clear
        let non_canonical = 0x0000_8000_0000_0000_usize;
        let result = UserspacePtr::<u8>::try_from_usize(non_canonical);
        assert!(result.is_err());
    }

    #[test]
    fn mut_ptr_rejects_non_canonical_address() {
        let result = UserspaceMutPtr::<u8>::try_from_usize(0x0000_8000_0000_0000);
        assert!(result.is_err());
    }

    #[test]
    fn mut_ptr_rejects_upper_half_address() {
        let result = UserspaceMutPtr::<u8>::try_from_usize(0xFFFF_8000_0000_0000);
        assert!(result.is_err());
    }

    #[test]
    fn test_address_just_below_boundary() {
        // One byte below the boundary
        let just_below = 0x0000_7FFF_FFFF_FFFE_usize;
        let result = UserspacePtr::<u8>::try_from_usize(just_below);
        assert!(result.is_ok());

        let ptr = result.unwrap();
        // Two bytes reach 0x0000_7FFF_FFFF_FFFF, the last lower-half byte
        assert!(ptr.validate_range(2).is_ok());
        // The third byte 0x0000_8000_0000_0000 is non-canonical.
        assert!(ptr.validate_range(3).is_err());
    }

    #[test]
    fn test_address_ranges_in_valid_userspace() {
        // Test various addresses in the valid userspace range
        let valid_addrs = [
            0x0000_0000_0000_0000_usize,
            0x0000_0000_0000_1000_usize,
            0x0000_0000_1000_0000_usize,
            0x0000_0001_0000_0000_usize,
            0x0000_1000_0000_0000_usize,
            0x0000_7FFF_0000_0000_usize,
            0x0000_7FFF_FFFF_0000_usize,
        ];

        for &addr in &valid_addrs {
            let result = UserspacePtr::<u8>::try_from_usize(addr);
            assert!(result.is_ok(), "Address 0x{:016x} should be valid", addr);
        }
    }
}
