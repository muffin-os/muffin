#![no_std]

extern crate alloc;

use iter::*;
use segment::*;

mod iter;
mod segment;

pub struct KernelSlab<V, const N: usize> {
    head: Segment<V, N>,
}

struct Index {
    segment: usize,
    offset: usize,
}

impl<V, const N: usize> KernelSlab<V, N> {
    pub fn new() -> Self
    where
        V: Default,
    {
        Self {
            head: Segment::new(),
        }
    }

    pub fn try_get(&self, index: usize) -> Option<&V> {
        let index = Index {
            segment: index / N,
            offset: index % N,
        };

        let segment = {
            let mut current = &self.head;
            for _ in 0..index.segment {
                current = current.try_next()?;
            }
            current
        };
        Some(&segment[index.offset])
    }

    pub fn get(&self, index: usize) -> &V
    where
        V: Default,
    {
        let index = Index {
            segment: index / N,
            offset: index % N,
        };

        let segment = {
            let mut current = &self.head;
            for _ in 0..index.segment {
                current = current.next();
            }
            current
        };
        &segment[index.offset]
    }

    pub fn iter(&self) -> Iter<'_, V, N> {
        Iter::new(&self.head)
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use std::sync::Mutex;

    extern crate std;

    use super::*;

    #[test]
    fn test_slab() {
        let slab = KernelSlab::<Mutex<String>, 2>::new();
        slab.get(0).lock().unwrap().push_str("0");
        slab.get(1).lock().unwrap().push_str("1");

        assert!(slab.try_get(2).is_none());
        assert!(slab.try_get(3).is_none());

        slab.get(2).lock().unwrap().push_str("2");
        slab.get(3).lock().unwrap().push_str("3");

        for i in 0..4 {
            assert_eq!(&format!("{i}"), &*slab.try_get(i).unwrap().lock().unwrap());
        }
    }

    #[test]
    fn test_iter() {
        let slab = KernelSlab::<Mutex<String>, 20>::new();
        for i in 0..35 {
            slab.get(i).lock().unwrap().push_str("hello");
        }
        let (index, _) = slab
            .iter()
            .enumerate()
            .filter(|(_, e)| e.lock().unwrap().len() == 0)
            .next()
            .unwrap();
        assert_eq!(35, index);

        let num_empty = slab.iter().filter(|e| e.lock().unwrap().is_empty()).count();
        assert_eq!(5, num_empty);
    }
}
