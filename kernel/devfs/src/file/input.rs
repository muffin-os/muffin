use alloc::collections::VecDeque;

use kernel_abi::{InputEvent, InputEventKind, SyncCode};

/// This is the event queue of one open input device file.
///
/// When a reader stops reading, the queue becomes full and loses events.
/// Then the queue discards its oldest batch as a whole. It keeps a
/// [`SyncCode::EventsLost`] event at the front. The value of this event is
/// the number of events that the queue discards after the last read of the
/// front event. Thus the reader always gets the number of lost events first,
/// and then the newest complete batches. The oldest events go first because
/// the newest events still show the current state. A batch goes as a whole
/// because half a batch has no meaning.
pub struct InputQueue {
    events: VecDeque<InputEvent>,
    capacity: usize,
}

impl InputQueue {
    /// The `capacity` must hold the loss counter and one event. Thus it must be
    /// 2 or more.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 2, "an input queue holds at least 2 events");
        Self {
            events: VecDeque::new(),
            capacity,
        }
    }

    pub fn push(&mut self, event: InputEvent) {
        if self.events.len() == self.capacity {
            self.discard_oldest_batch();
        }
        self.events.push_back(event);
    }

    pub fn pop(&mut self) -> Option<InputEvent> {
        self.events.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Removes the events up to and including the first batch end. Adds their
    /// number to the loss counter at the front. This frees one or more slots.
    fn discard_oldest_batch(&mut self) {
        let mut lost = match self.events.front().copied() {
            Some(front) if front.matches(InputEventKind::Sync, SyncCode::EventsLost) => {
                self.events.pop_front();
                front.value
            }
            _ => 0,
        };
        while let Some(event) = self.events.pop_front() {
            lost = lost.saturating_add(1);
            if event.matches(InputEventKind::Sync, SyncCode::BatchEnd) {
                break;
            }
        }
        self.events
            .push_front(InputEvent::sync(SyncCode::EventsLost, lost));
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use kernel_abi::MotionAxis;

    use super::*;

    fn motion(value: i32) -> InputEvent {
        InputEvent {
            event_type: InputEventKind::Motion.into(),
            code: MotionAxis::X.into(),
            value,
        }
    }

    fn batch_end() -> InputEvent {
        InputEvent::sync(SyncCode::BatchEnd, 0)
    }

    fn lost(count: i32) -> InputEvent {
        InputEvent::sync(SyncCode::EventsLost, count)
    }

    fn drain(queue: &mut InputQueue) -> Vec<InputEvent> {
        core::iter::from_fn(|| queue.pop()).collect()
    }

    #[test]
    fn pops_in_push_order_until_empty() {
        let mut queue = InputQueue::new(4);
        assert!(queue.is_empty());
        queue.push(motion(1));
        queue.push(motion(2));
        assert!(!queue.is_empty());
        assert_eq!(drain(&mut queue), [motion(1), motion(2)]);
        assert!(queue.is_empty());
    }

    #[test]
    fn a_full_queue_still_takes_the_last_event_without_a_loss() {
        let mut queue = InputQueue::new(3);
        for value in 1..=3 {
            queue.push(motion(value));
        }
        assert_eq!(drain(&mut queue), [motion(1), motion(2), motion(3)]);
    }

    #[test]
    fn overflow_discards_the_oldest_whole_batch_and_counts_it() {
        let mut queue = InputQueue::new(6);
        for event in [motion(1), motion(2), batch_end(), motion(3), batch_end()] {
            queue.push(event);
        }
        queue.push(motion(4));
        queue.push(motion(5));
        assert_eq!(
            drain(&mut queue),
            [lost(3), motion(3), batch_end(), motion(4), motion(5)]
        );
    }

    #[test]
    fn repeated_overflow_accumulates_the_count_in_one_marker() {
        let mut queue = InputQueue::new(4);
        for event in [motion(1), batch_end(), motion(2), batch_end()] {
            queue.push(event);
        }
        queue.push(motion(3));
        assert_eq!(queue.events.front(), Some(&lost(2)));
        queue.push(batch_end());
        queue.push(motion(4));
        assert_eq!(
            drain(&mut queue),
            [lost(4), motion(3), batch_end(), motion(4)]
        );
    }

    #[test]
    fn a_batch_longer_than_the_queue_is_lost_whole() {
        let mut queue = InputQueue::new(3);
        for value in 1..=5 {
            queue.push(motion(value));
        }
        // Events 1 to 3 fill the queue. Event 4 discards them as one open batch.
        // Event 5 fits.
        assert_eq!(drain(&mut queue), [lost(3), motion(4), motion(5)]);
    }

    #[test]
    fn a_read_of_the_marker_starts_a_new_count() {
        let mut queue = InputQueue::new(3);
        for event in [motion(1), batch_end(), motion(2), motion(3)] {
            queue.push(event);
        }
        assert_eq!(queue.pop(), Some(lost(2)));
        queue.push(batch_end());
        queue.push(motion(4));
        assert_eq!(drain(&mut queue), [lost(3), motion(4)]);
    }
}
