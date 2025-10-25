//! Utilities for creating properly aligned byte buffers for ELF parsing.

use alloc::vec;
use alloc::vec::Vec;

/// A fixed-size aligned byte buffer for ELF headers with 8-byte alignment.
///
/// ELF headers are exactly 64 bytes in size for 64-bit ELF files.
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

/// Creates a properly aligned byte buffer for variable-size data using a stack-allocated aligned array.
///
/// This is a helper type that provides 8-byte aligned storage for variable-size ELF data.
#[repr(align(8))]
pub struct AlignedBuffer<const N: usize>([u8; N]);

impl<const N: usize> AlignedBuffer<N> {
    /// Creates a new zero-initialized aligned buffer.
    pub fn new() -> Self {
        Self([0u8; N])
    }
}

impl<const N: usize> Default for AlignedBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> AsRef<[u8]> for AlignedBuffer<N> {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl<const N: usize> AsMut<[u8]> for AlignedBuffer<N> {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl<const N: usize> core::ops::Index<usize> for AlignedBuffer<N> {
    type Output = u8;
    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl<const N: usize> core::ops::IndexMut<usize> for AlignedBuffer<N> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl<const N: usize> core::ops::Index<core::ops::Range<usize>> for AlignedBuffer<N> {
    type Output = [u8];
    fn index(&self, index: core::ops::Range<usize>) -> &Self::Output {
        &self.0[index]
    }
}

impl<const N: usize> core::ops::IndexMut<core::ops::Range<usize>> for AlignedBuffer<N> {
    fn index_mut(&mut self, index: core::ops::Range<usize>) -> &mut Self::Output {
        &mut self.0[index]
    }
}

/// A wrapper around Vec<u64> that provides properly aligned byte storage for variable-size data.
///
/// This is used in tests where the size isn't known at compile time.
pub struct AlignedVec {
    inner: Vec<u64>,
    len: usize,
}

impl AlignedVec {
    /// Creates a new aligned vector with the given byte size.
    pub fn new(size: usize) -> Self {
        let num_u64s = size.div_ceil(8);
        Self {
            inner: vec![0u64; num_u64s],
            len: size,
        }
    }

    /// Returns a byte slice view of the vector.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.inner.as_ptr() as *const u8, self.len) }
    }

    /// Returns a mutable byte slice view of the vector.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.inner.as_mut_ptr() as *mut u8, self.len) }
    }
}

impl AsRef<[u8]> for AlignedVec {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl AsMut<[u8]> for AlignedVec {
    fn as_mut(&mut self) -> &mut [u8] {
        self.as_bytes_mut()
    }
}
