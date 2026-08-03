use core::fmt::{Display, Formatter};
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct TaskId(u64);

impl Display for TaskId {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl<T> PartialEq<T> for TaskId
where
    T: Into<u64> + Copy,
{
    fn eq(&self, other: &T) -> bool {
        self.0 == (*other).into()
    }
}

impl !Default for TaskId {}

impl TaskId {
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        TaskId(COUNTER.fetch_add(1, Relaxed))
    }
}
