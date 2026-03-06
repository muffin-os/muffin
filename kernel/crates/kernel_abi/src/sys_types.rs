use core::fmt::{Display, Formatter};

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
}
