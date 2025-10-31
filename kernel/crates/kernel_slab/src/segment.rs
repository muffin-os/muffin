use alloc::boxed::Box;
use core::fmt::Debug;
use core::ops::Deref;
use core::ptr::null_mut;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::Ordering::{Acquire, Relaxed, SeqCst};

pub(crate) struct Segment<V, const N: usize> {
    elements: [V; N],
    pub(crate) next_segment: AtomicPtr<Self>,
}

impl<V, const N: usize> Segment<V, N>
where
    V: Default,
{
    pub fn new() -> Self {
        Self {
            elements: core::array::from_fn(|_| V::default()),
            next_segment: AtomicPtr::new(null_mut()),
        }
    }

    pub fn next(&self) -> &Self {
        if let Some(next) = self.try_next() {
            next
        } else {
            // we need to optimistically allocate a new segment, which we drop
            // if someone else is also currently allocating and stores it faster
            // than us
            let newly_allocated = Box::new(Self::new());
            let newly_allocated_ptr = Box::into_raw(newly_allocated);
            let ptr = match self.next_segment.compare_exchange(
                null_mut(),
                newly_allocated_ptr,
                Acquire,
                Relaxed,
            ) {
                Ok(_) => newly_allocated_ptr,
                Err(_) => {
                    let _ = unsafe {
                        // Safety: this is safe because we know that we just allocated the memory
                        // behind the pointer, and we didn't store it in the next ptr
                        Box::from_raw(newly_allocated_ptr)
                    };
                    // someone else stored before we could, so we need to re-read the next ptr
                    self.next_segment.load(Acquire)
                }
            };
            unsafe {
                // Safety: this is safe because the segment can't be dropped while
                // this `&self` exists
                &*ptr
            }
        }
    }
}

impl<V, const N: usize> Segment<V, N> {
    pub fn try_next(&self) -> Option<&Self> {
        let next = self.next_segment.load(Acquire);
        if next.is_null() {
            None
        } else {
            unsafe {
                // Safety: this is safe because the segment can't be dropped while
                // this `&self` exists
                Some(&*next)
            }
        }
    }
}

impl<V, const N: usize> Deref for Segment<V, N> {
    type Target = [V; N];

    fn deref(&self) -> &Self::Target {
        &self.elements
    }
}

impl<V, const N: usize> Drop for Segment<V, N> {
    fn drop(&mut self) {
        let next = self.next_segment.swap(null_mut(), SeqCst);
        if next.is_null() {
            return;
        }

        let next = unsafe {
            // Safety: this is safe because we have exclusive access,
            // so as long as the lifetime of all passed out references
            // are bound to `self`, this is safe.
            Box::from_raw(next)
        };
        drop(next);
    }
}

impl<V, const N: usize> PartialEq for Segment<V, N>
where
    V: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.elements == other.elements
    }
}

impl<V, const N: usize> Eq for Segment<V, N> where V: Eq {}

impl<V, const N: usize> Debug for Segment<V, N>
where
    V: Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Segment")
            .field("elements", &self.elements)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use core::cell::RefCell;

    use super::*;

    #[test]
    fn test_new() {
        let segment = Segment::<String, 10>::new();
        for i in 0..10 {
            assert_eq!("", segment[i]);
        }
    }

    #[test]
    fn test_next_try_next() {
        let segment = Segment::<String, 10>::new();
        assert_eq!(None, segment.try_next());
        let next_segment = segment.next();
        assert_eq!(&segment, next_segment); // `next_segment` ptr is not part of `eq`
    }

    #[test]
    fn test_index() {
        let segment = Segment::<RefCell<String>, 10>::new();
        segment[4].borrow_mut().push_str("hello");
        for i in (0..4).chain(5..10) {
            assert_eq!("", &*segment[i].borrow());
        }
        assert_eq!("hello", &*segment[4].borrow());
    }
}
