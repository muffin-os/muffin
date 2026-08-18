use core::fmt::{Display, Formatter};
use core::marker::PhantomData;

#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ProcessId(u64);

impl Display for ProcessId {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl<T> From<T> for ProcessId
where
    T: Into<u64> + Copy,
{
    fn from(value: T) -> Self {
        Self(value.into())
    }
}

impl<T> PartialEq<T> for ProcessId
where
    T: Into<u64> + Copy,
{
    fn eq(&self, other: &T) -> bool {
        self.0 == (*other).into()
    }
}

impl !Default for ProcessId {}

impl ProcessId {
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct StrSlice<'a> {
    ptr: usize,
    len: usize,
    _life: PhantomData<&'a str>,
}

impl<'a> From<&'a str> for StrSlice<'a> {
    fn from(s: &'a str) -> Self {
        Self {
            ptr: s.as_ptr() as usize,
            len: s.len(),
            _life: PhantomData,
        }
    }
}

impl StrSlice<'_> {
    /// # Safety
    ///
    /// For nonzero `len`, `ptr..ptr + len` must stay valid and unchanged for
    /// the chosen lifetime.
    #[must_use]
    pub const unsafe fn from_raw(ptr: usize, len: usize) -> Self {
        Self {
            ptr,
            len,
            _life: PhantomData,
        }
    }

    #[must_use]
    pub const fn ptr(&self) -> usize {
        self.ptr
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}
