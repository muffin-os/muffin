use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use crate::DeviceEvent;

/// A ring buffer of events between one or more fillers and a consumer on
/// other threads. Share it through an `Arc`.
///
/// The buffer holds at most `capacity` events. When the buffer is full, a
/// push removes the oldest event to make space for the new event. Thus a
/// filler never blocks, and a slow consumer loses the oldest events, not the
/// newest. A lost release can make the consumer think that a key or a button
/// stays down.
pub struct InputBuffer {
    events: Mutex<VecDeque<DeviceEvent>>,
    capacity: NonZeroUsize,
    pushed: Condvar,
}

impl InputBuffer {
    /// Makes an empty buffer that holds at most `capacity` events.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            events: Mutex::new(VecDeque::with_capacity(capacity.get())),
            capacity,
            pushed: Condvar::new(),
        }
    }

    pub fn capacity(&self) -> NonZeroUsize {
        self.capacity
    }

    /// Adds `event` after the newest event. When the buffer is full, this
    /// removes the oldest event first.
    pub fn push(&self, event: DeviceEvent) {
        self.extend([event]);
    }

    /// Adds the events in sequence with one lock and one wakeup. When the
    /// buffer becomes full, each event removes the oldest event first.
    pub fn extend(&self, events: impl IntoIterator<Item = DeviceEvent>) {
        let mut queue = self.lock();
        for event in events {
            if queue.len() == self.capacity.get() {
                queue.pop_front();
            }
            queue.push_back(event);
        }
        drop(queue);
        self.pushed.notify_all();
    }

    /// Removes and returns the oldest event. Blocks until an event is
    /// present.
    pub fn pop(&self) -> DeviceEvent {
        let mut queue = self
            .pushed
            .wait_while(self.lock(), |queue| queue.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        queue
            .pop_front()
            .expect("wait_while returns only for a queue that is not empty")
    }

    /// Removes and returns the oldest event. Returns `None` at once when the
    /// buffer is empty.
    pub fn try_pop(&self) -> Option<DeviceEvent> {
        self.lock().pop_front()
    }

    /// Moves all the events of the buffer into `out`, oldest first. Does
    /// not block. An empty buffer adds nothing.
    pub fn drain(&self, out: &mut impl Extend<DeviceEvent>) {
        out.extend(self.lock().drain(..));
    }

    /// A panic of a caller's iterator can poison the lock. Each `VecDeque`
    /// operation completes or does nothing, so the queue stays consistent.
    fn lock(&self) -> MutexGuard<'_, VecDeque<DeviceEvent>> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::thread;

    use super::*;
    use crate::{DeviceId, Event};

    fn wheel(delta: i32) -> DeviceEvent {
        DeviceEvent {
            device: DeviceId(0),
            event: Event::Wheel { delta },
        }
    }

    fn buffer(capacity: usize) -> InputBuffer {
        InputBuffer::new(NonZeroUsize::new(capacity).unwrap())
    }

    fn drained(buffer: &InputBuffer) -> Vec<DeviceEvent> {
        let mut out = Vec::new();
        buffer.drain(&mut out);
        out
    }

    #[test]
    fn order_stays_across_wrap_around() {
        let buffer = buffer(3);
        for round in 0..5 {
            let base = round * 2;
            buffer.push(wheel(base));
            buffer.push(wheel(base + 1));
            assert_eq!(buffer.try_pop(), Some(wheel(base)));
            assert_eq!(buffer.try_pop(), Some(wheel(base + 1)));
        }
        assert_eq!(buffer.try_pop(), None);
    }

    #[test]
    fn full_buffer_drops_the_oldest_event() {
        let buffer = buffer(3);
        buffer.extend((0..5).map(wheel));
        buffer.push(wheel(5));
        assert_eq!(drained(&buffer), [wheel(3), wheel(4), wheel(5)]);
        assert_eq!(drained(&buffer), []);
    }

    #[test]
    fn pop_wakes_on_a_push_from_another_thread() {
        let buffer = Arc::new(buffer(1));
        let (started, is_started) = mpsc::channel();
        let consumer = thread::spawn({
            let buffer = buffer.clone();
            move || {
                started.send(()).unwrap();
                buffer.pop()
            }
        });
        is_started.recv().unwrap();
        buffer.push(wheel(7));
        assert_eq!(consumer.join().unwrap(), wheel(7));
        assert_eq!(buffer.try_pop(), None);
    }
}
