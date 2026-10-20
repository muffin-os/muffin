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
        // Safety: try_from_usize places no obligation on the caller.
        unsafe { Self::try_from_usize(ptr as usize) }
    }
}

impl<T> UserspacePtr<T> {
    /// # Errors
    /// Returns [`NotUserspace`] if `ptr` is non-canonical or in the upper half.
    ///
    /// # Safety
    /// None. The address is only inspected, never dereferenced.
    // TODO(tpdenk): make this a safe fn and drop the `unsafe {}` blocks around
    // the callers in kernel/boot.
    pub unsafe fn try_from_usize(ptr: usize) -> Result<Self, NotUserspace> {
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
    /// The end address `addr + size` is checked as an address in its own
    /// right, so a range that ends exactly at the top of the lower half is
    /// rejected, as is one whose end overflows.
    pub fn validate_range(&self, size: usize) -> Result<(), NotUserspace> {
        let start = self.addr();
        let end = start.checked_add(size).ok_or(NotUserspace(start))?;

        if is_upper_half(end)? {
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
///
/// Uses VirtAddr to validate that the address is canonical and then checks
/// if it's in the upper half. On x86_64 with 4-level paging:
/// - Lower half (userspace): 0x0000_0000_0000_0000 to 0x0000_7FFF_FFFF_FFFF
/// - Upper half (kernel):    0xFFFF_8000_0000_0000 to 0xFFFF_FFFF_FFFF_FFFF
///
/// Returns an error if the address is not canonical.
#[inline]
fn is_upper_half(addr: usize) -> Result<bool, NotUserspace> {
    // Use VirtAddr to check if the address is canonical
    let virt_addr = VirtAddr::try_new(addr as u64).map_err(|_| NotUserspace(addr))?;

    // Check if it's in the upper half (kernel space)
    // Upper half starts at 0xFFFF_8000_0000_0000
    Ok(virt_addr.as_u64() >= 0xFFFF_8000_0000_0000)
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
        // Safety: try_from_usize places no obligation on the caller.
        unsafe { Self::try_from_usize(ptr as usize) }
    }
}

impl<T> !Clone for UserspaceMutPtr<T> {}

impl<T> UserspaceMutPtr<T> {
    /// # Errors
    /// Returns [`NotUserspace`] if `ptr` is non-canonical or in the upper half.
    ///
    /// # Safety
    /// None. The address is only inspected, never dereferenced.
    // TODO(tpdenk): make this a safe fn and drop the `unsafe {}` blocks around
    // the callers in kernel/boot.
    pub unsafe fn try_from_usize(ptr: usize) -> Result<Self, NotUserspace> {
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
    /// The end address `addr + size` is checked as an address in its own
    /// right, so a range that ends exactly at the top of the lower half is
    /// rejected, as is one whose end overflows.
    pub fn validate_range(&self, size: usize) -> Result<(), NotUserspace> {
        let start = self.addr();
        let end = start.checked_add(size).ok_or(NotUserspace(start))?;

        if is_upper_half(end)? {
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
        // 0x0000_7FFF_FFFF_FFFF, the last lower-half address
        let max_lower_half = (1_usize << 47) - 1;
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(max_lower_half).unwrap() };
        assert!(ptr.validate_range(0).is_ok());
        // The end address 0x0000_8000_0000_0000 is non-canonical.
        assert!(ptr.validate_range(1).is_err());
    }

    #[test]
    fn test_validate_range_overflow_into_upper_half() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0x0000_7FFF_FFFF_F000).unwrap() };
        // The end address 0x0000_8000_0000_1000 is non-canonical.
        assert!(ptr.validate_range(0x2000).is_err());
    }

    #[test]
    fn test_validate_range_arithmetic_overflow() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0x0000_7FFF_FFFF_FFFF).unwrap() };
        assert!(ptr.validate_range(usize::MAX).is_err());
    }

    #[test]
    fn test_validate_range_near_boundary() {
        // The lower half ends at 0x0000_7FFF_FFFF_FFFF.
        let base = 0x0000_7FFF_FFFF_F000_usize;
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(base).unwrap() };

        assert!(ptr.validate_range(0xFFF).is_ok());
        // base + 0x1000 = 0x0000_8000_0000_0000 is non-canonical
        assert!(ptr.validate_range(0x1000).is_err());
        assert!(ptr.validate_range(0x2000).is_err());
    }

    #[test]
    fn test_validate_range_from_zero() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(0).unwrap() };
        // Can map up to the entire lower half (48-bit addressing)
        let max_lower_half = (1_usize << 47) - 1;
        assert!(ptr.validate_range(max_lower_half).is_ok());
        // But not including the boundary (bit 47 set)
        assert!(ptr.validate_range(1_usize << 47).is_err());
    }

    #[test]
    fn test_validate_range_max_size() {
        let ptr = unsafe { UserspacePtr::<u8>::try_from_usize(1).unwrap() };
        // Maximum possible size without overflow
        assert!(ptr.validate_range(usize::MAX - 1).is_err());
    }

    #[test]
    fn test_canonical_address_lower_half_max() {
        // Maximum canonical lower-half address (bit 47 = 0)
        let max_canonical_lower = 0x0000_7FFF_FFFF_FFFF_usize;
        let result = unsafe { UserspacePtr::<u8>::try_from_usize(max_canonical_lower) };
        assert!(result.is_ok());
    }

    #[test]
    fn test_canonical_address_upper_half_start() {
        // Minimum canonical upper-half address (bit 47 = 1, bits 48-63 must be 1)
        // This is 0xFFFF_8000_0000_0000, but we should reject it as it's kernel space
        let min_canonical_upper = 0xFFFF_8000_0000_0000_usize;
        let result = unsafe { UserspacePtr::<u8>::try_from_usize(min_canonical_upper) };
        assert!(result.is_err());
    }

    #[test]
    fn test_non_canonical_address_rejected() {
        // bit 47 set while bits 48-63 are clear
        let non_canonical = 0x0000_8000_0000_0000_usize;
        let result = unsafe { UserspacePtr::<u8>::try_from_usize(non_canonical) };
        assert!(result.is_err());
    }

    #[test]
    fn mut_ptr_rejects_non_canonical_address() {
        let result = unsafe { UserspaceMutPtr::<u8>::try_from_usize(0x0000_8000_0000_0000) };
        assert!(result.is_err());
    }

    #[test]
    fn mut_ptr_rejects_upper_half_address() {
        let result = unsafe { UserspaceMutPtr::<u8>::try_from_usize(0xFFFF_8000_0000_0000) };
        assert!(result.is_err());
    }

    #[test]
    fn test_address_just_below_boundary() {
        // One byte below the boundary
        let just_below = 0x0000_7FFF_FFFF_FFFE_usize;
        let result = unsafe { UserspacePtr::<u8>::try_from_usize(just_below) };
        assert!(result.is_ok());

        let ptr = result.unwrap();
        // Should be able to access 1 byte (up to 0x0000_7FFF_FFFF_FFFF)
        assert!(ptr.validate_range(1).is_ok());
        // The end address 0x0000_8000_0000_0000 is non-canonical.
        assert!(ptr.validate_range(2).is_err());
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
            let result = unsafe { UserspacePtr::<u8>::try_from_usize(addr) };
            assert!(result.is_ok(), "Address 0x{:016x} should be valid", addr);
        }
    }
}
