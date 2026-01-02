use core::fmt::{Display, Formatter};
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ProcessId(u64);

impl Display for ProcessId {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Display::fmt(&self.0, f)
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
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        ProcessId(COUNTER.fetch_add(1, Relaxed))
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0 == 0
    }
}
