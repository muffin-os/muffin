//! Utilities for creating properly aligned byte buffers for ELF parsing.
//!
//! The zerocopy library's `try_ref_from_bytes` requires proper alignment for the target type.
//! ElfHeader contains `usize` fields requiring 8-byte alignment on 64-bit systems.

use alloc::vec;
use alloc::vec::Vec;

/// A fixed-size aligned byte buffer for ELF headers (64 bytes with 8-byte alignment).
///
/// This ensures proper alignment for zerocopy's `try_ref_from_bytes` function when parsing ELF headers.
#[repr(align(8))]
#[derive(Debug, Clone)]
pub struct AlignedElfData([u8; 64]);

impl AlignedElfData {
    /// Creates a new zero-initialized aligned ELF data buffer.
    pub fn new() -> Self {
        Self([0u8; 64])
    }

    /// Creates an aligned ELF data buffer from a 64-byte array.
    pub fn from_array(data: [u8; 64]) -> Self {
        Self(data)
    }
}

impl Default for AlignedElfData {
    fn default() -> Self {
        Self::new()
    }
}

impl AsRef<[u8]> for AlignedElfData {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsMut<[u8]> for AlignedElfData {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl core::ops::Index<usize> for AlignedElfData {
    type Output = u8;
    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl core::ops::IndexMut<usize> for AlignedElfData {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl core::ops::Index<core::ops::Range<usize>> for AlignedElfData {
    type Output = [u8];
    fn index(&self, index: core::ops::Range<usize>) -> &Self::Output {
        &self.0[index]
    }
}

impl core::ops::IndexMut<core::ops::Range<usize>> for AlignedElfData {
    fn index_mut(&mut self, index: core::ops::Range<usize>) -> &mut Self::Output {
        &mut self.0[index]
    }
}

/// Helper to create an aligned Vec for variable-size ELF data.
///
/// Allocates as `Vec<u64>` to ensure 8-byte alignment, which can then be safely
/// reinterpreted as bytes for zerocopy operations.
pub fn create_aligned_vec(size: usize) -> Vec<u64> {
    let num_u64s = (size + 7) / 8; // Round up to nearest u64
    vec![0u64; num_u64s]
}

/// Helper to get a byte slice from an aligned vec with proper size.
///
/// # Safety
/// The caller must ensure that `size` does not exceed the actual byte capacity of the vec.
pub fn aligned_vec_as_bytes(vec: &[u64], size: usize) -> &[u8] {
    unsafe { core::slice::from_raw_parts(vec.as_ptr() as *const u8, size) }
}

/// Helper to get a mutable byte slice from an aligned vec with proper size.
///
/// # Safety
/// The caller must ensure that `size` does not exceed the actual byte capacity of the vec.
pub fn aligned_vec_as_bytes_mut(vec: &mut [u64], size: usize) -> &mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(vec.as_mut_ptr() as *mut u8, size) }
}
