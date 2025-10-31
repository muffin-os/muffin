use crate::segment::Segment;

pub struct Iter<'a, V, const N: usize> {
    segment: &'a Segment<V, N>,
    index: usize,
}

impl<'a, V, const N: usize> Iter<'a, V, N> {
    pub(crate) fn new(segment: &'a Segment<V, N>) -> Self {
        Self { segment, index: 0 }
    }
}

impl<'a, V, const N: usize> Iterator for Iter<'a, V, N> {
    type Item = &'a V;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= N {
            // end of segment, get next segment
            self.segment = self.segment.try_next()?;
            // reset index after loading next segment, because that might return early
            self.index = 0;
        }
        let result = Some(&self.segment[self.index]);
        self.index += 1;
        result
    }
}
