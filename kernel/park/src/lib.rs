//! A lock-free parking lot for values whose owner must hand them off in a
//! context where neither locking nor allocation is available.
//!
//! A task that must block reserves a slot, keeps the [`ParkTicket`] where
//! the scheduler finds it, and hands the [`UnparkTicket`] to the subsystem
//! that will wake it. The scheduler parks the task after the context
//! switch with interrupts disabled, so [`ParkTicket::park`] neither locks nor
//! allocates. The waking subsystem unparks it and moves it to the run
//! queue. The two sides race, so the unpark may run first.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::fmt::{Debug, Formatter};
use core::mem::MaybeUninit;
use core::ptr::{from_ref, null_mut};
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use core::sync::atomic::{AtomicPtr, AtomicU8};

use thiserror::Error;

/// Slots in every segment, including the inline head segment.
pub const SEGMENT_SLOTS: usize = 64;

#[repr(u8)]
enum State {
    Empty,
    Reserved,
    Occupied,
    Unparked,
}

struct Slot<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Slot<T> {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(State::Empty as u8),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// Safety: the cell is written only by the single `ParkTicket` for the slot while
// the state is `Reserved` or `Unparked`, and read only by whichever of
// `park` and `unpark` observes the other's state claim, ordered by the
// release/acquire pair on `state`. No `&T` is ever handed out, so only
// ownership of `T` crosses threads and `T: Sync` is not required.
unsafe impl<T: Send> Sync for Slot<T> {}

/// A segment is never freed while the lot is alive, so a slot address stays
/// valid for as long as the lot does. That is what lets a ticket hold a
/// borrow of its slot instead of an index, and it is why the lot grows by
/// appending segments rather than by reallocating one contiguous buffer.
struct Segment<T> {
    slots: [Slot<T>; SEGMENT_SLOTS],
    next: AtomicPtr<Segment<T>>,
}

impl<T> Segment<T> {
    const fn new() -> Self {
        Self {
            slots: [const { Slot::new() }; SEGMENT_SLOTS],
            next: AtomicPtr::new(null_mut()),
        }
    }

    fn next_or_append(&self) -> &Self {
        let existing = self.next.load(Acquire);
        if !existing.is_null() {
            // Safety: a published successor stays allocated until the lot is
            // dropped, which cannot happen while `self` is borrowed.
            return unsafe { &*existing };
        }

        let fresh = Box::into_raw(Box::new(Self::new()));
        match self
            .next
            .compare_exchange(null_mut(), fresh, AcqRel, Acquire)
        {
            // Safety: the exchange published `fresh` and nothing frees it
            // while the lot is alive.
            Ok(_) => unsafe { &*fresh },
            Err(winner) => {
                // Safety: `fresh` was never published, so this is the only
                // pointer to that allocation and it came from `Box::into_raw`.
                drop(unsafe { Box::from_raw(fresh) });
                // Safety: same as the published-successor case above.
                unsafe { &*winner }
            }
        }
    }

    /// Drops the value of every occupied slot. `Reserved` and `Unparked`
    /// slots hold no value that anybody owns, so they are skipped.
    fn drop_values(&mut self) {
        for slot in &mut self.slots {
            if *slot.state.get_mut() == State::Occupied as u8 {
                // Safety: the state says the value is initialized, and the
                // exclusive borrow rules out any ticket or concurrent access.
                unsafe { (*slot.value.get()).assume_init_drop() };
            }
        }
    }
}

/// A set of slots that values can be parked in and retrieved from.
///
/// The head segment is inline, so [`ParkingLot::new`] is `const` and a lot can
/// live in a `static` without lazy initialization.
pub struct ParkingLot<T> {
    head: Segment<T>,
}

impl<T> ParkingLot<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            head: Segment::new(),
        }
    }

    /// Claims a free slot and returns its reservation.
    ///
    /// Scans slots linearly and skips the ones in use, so the cost grows with
    /// the number of slots currently in use. May allocate a fresh segment when
    /// every existing slot is taken, but never locks.
    #[must_use = "dropping a reservation leaves the slot reserved for the lifetime of the lot"]
    pub fn reserve(&self) -> Reservation<'_, T> {
        let mut segment = &self.head;
        loop {
            if let Some(reservation) = Self::claim(segment) {
                return reservation;
            }
            segment = segment.next_or_append();
        }
    }

    /// Claims a free slot like [`ParkingLot::reserve`], but returns `None`
    /// instead of appending a segment when every existing slot is taken.
    ///
    /// Never allocates and never locks, so this is the only reservation that
    /// is legal in interrupt context.
    #[must_use = "dropping a reservation leaves the slot reserved for the lifetime of the lot"]
    pub fn try_reserve(&self) -> Option<Reservation<'_, T>> {
        let mut segment = &self.head;
        loop {
            if let Some(reservation) = Self::claim(segment) {
                return Some(reservation);
            }
            let next = segment.next.load(Acquire);
            if next.is_null() {
                return None;
            }
            // Safety: a published successor stays allocated until the lot is
            // dropped, which cannot happen while `self` is borrowed.
            segment = unsafe { &*next };
        }
    }

    fn claim(segment: &Segment<T>) -> Option<Reservation<'_, T>> {
        for slot in &segment.slots {
            if slot.state.load(Relaxed) != State::Empty as u8 {
                continue;
            }
            if slot
                .state
                .compare_exchange(State::Empty as u8, State::Reserved as u8, Acquire, Relaxed)
                .is_ok()
            {
                return Some(Reservation { slot });
            }
        }
        None
    }
}

impl<T> Default for ParkingLot<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Debug for ParkingLot<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ParkingLot").finish_non_exhaustive()
    }
}

impl<T> Drop for ParkingLot<T> {
    /// The exclusive borrow proves no ticket is alive and no other thread can
    /// touch a slot, so every access below is unsynchronized on purpose.
    ///
    /// The walk is iterative because the chain can be long and one stack frame
    /// per segment would overflow the kernel stack.
    fn drop(&mut self) {
        self.head.drop_values();
        let mut next = *self.head.next.get_mut();
        while !next.is_null() {
            // Safety: the pointer was published by `next_or_append` from
            // `Box::into_raw` and is unlinked from no other owner, so this is
            // the only reclamation of that allocation.
            let mut segment = unsafe { Box::from_raw(next) };
            segment.drop_values();
            next = *segment.next.get_mut();
        }
    }
}

/// An unsplit claim on one slot, holding both of its ticket rights.
///
/// While a reservation exists neither ticket has been created, so the slot
/// is `Reserved` and no other party can act on it. That exclusivity is what
/// makes [`Reservation::release`] sound, and it pairs the two tickets of
/// [`Reservation::split`] to the same slot by construction.
#[must_use = "dropping a reservation leaves the slot reserved for the lifetime of the lot"]
pub struct Reservation<'a, T> {
    slot: &'a Slot<T>,
}

impl<'a, T> Reservation<'a, T> {
    /// Creates the park and unpark tickets of the reserved slot.
    pub fn split(self) -> (ParkTicket<'a, T>, UnparkTicket<'a, T>) {
        (
            ParkTicket { slot: self.slot },
            UnparkTicket { slot: self.slot },
        )
    }

    /// Returns the slot to the lot unused.
    pub fn release(self) {
        // Both ticket rights are surrendered with the state still `Reserved`,
        // so no other party can race this store.
        self.slot.state.store(State::Empty as u8, Release);
    }
}

impl<T> Debug for Reservation<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Reservation").finish_non_exhaustive()
    }
}

/// The right to park one value in a reserved slot, consumed on use so a second
/// park is unrepresentable.
#[must_use]
pub struct ParkTicket<'a, T> {
    slot: &'a Slot<T>,
}

impl<T> ParkTicket<'_, T> {
    /// Parks `value` in the reserved slot.
    ///
    /// Returns [`ParkError::Unparked`] carrying `value` back when the
    /// unpark side already ran, because the caller has nowhere else to put
    /// it. The scheduler then enqueues the task on the global run queue
    /// instead of parking it.
    pub fn park(self, value: T) -> Result<(), ParkError<T>> {
        // Safety: the slot is `Reserved` or `Unparked`, and in neither state
        // does another party touch the cell. `unpark` only claims the state
        // on its `Reserved` path, and `reserve` only claims `Empty` slots.
        unsafe { (*self.slot.value.get()).write(value) };

        if self
            .slot
            .state
            .compare_exchange(
                State::Reserved as u8,
                State::Occupied as u8,
                Release,
                Acquire,
            )
            .is_ok()
        {
            return Ok(());
        }

        // Safety: the exchange failed, so the state is `Unparked` and
        // `unpark` left the value alone. This reads back a write made on
        // this thread, and the store below releases the slot for reuse.
        let value = unsafe { (*self.slot.value.get()).assume_init_read() };
        self.slot.state.store(State::Empty as u8, Release);
        Err(ParkError::Unparked(value))
    }
}

impl<T> Debug for ParkTicket<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ParkTicket").finish_non_exhaustive()
    }
}

/// The right to retrieve the value of a reserved slot, consumed on use so a
/// second unpark is unrepresentable.
#[must_use]
pub struct UnparkTicket<'a, T> {
    slot: &'a Slot<T>,
}

impl<T> UnparkTicket<'_, T> {
    /// Retrieves the parked value, or `None` when the park side has not
    /// published yet. In that case the park fails with
    /// [`ParkError::Unparked`] and keeps ownership of the value.
    ///
    /// One exchange suffices with no retry, because the state can only be
    /// `Reserved` or `Occupied` here and neither can change under it.
    #[must_use]
    pub fn unpark(self) -> Option<T> {
        if self
            .slot
            .state
            .compare_exchange(
                State::Reserved as u8,
                State::Unparked as u8,
                Acquire,
                Acquire,
            )
            .is_ok()
        {
            return None;
        }

        // Safety: the exchange failed, so the state is `Occupied` and the
        // acquire failure load synchronizes with the release in `park`,
        // making the cell write visible. The store releases the slot for reuse.
        let value = unsafe { (*self.slot.value.get()).assume_init_read() };
        self.slot.state.store(State::Empty as u8, Release);
        Some(value)
    }
}

impl<T> Debug for UnparkTicket<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnparkTicket").finish_non_exhaustive()
    }
}

impl<T: Send> UnparkTicket<'static, T> {
    /// Wraps the ticket in a cloneable one-shot [`Waker`].
    ///
    /// Allocates, so this is task-context only. The `'static` lifetime is
    /// what makes storing the raw slot pointer sound: the slot lives as long
    /// as the lot, and the lot outlives every waker clone.
    #[must_use]
    pub fn into_waker(self) -> Waker<T> {
        Waker {
            slot: Arc::new(AtomicPtr::new(from_ref(self.slot).cast_mut())),
        }
    }
}

/// A cloneable one-shot handle to an [`UnparkTicket`]. Exactly one clone
/// gets to unpark the slot, so several wake sources can share one ticket
/// without coordination.
pub struct Waker<T: 'static> {
    slot: Arc<AtomicPtr<Slot<T>>>,
}

impl<T: Send> Waker<T> {
    /// Retrieves the parked value if this is the first clone to fire and the
    /// parker already parked it.
    ///
    /// `None` covers both "another clone already fired" and "the parker has
    /// not parked yet". In the latter case the parker's `park` fails
    /// with [`ParkError::Unparked`] and it keeps ownership of the value.
    pub fn wake(&self) -> Option<T> {
        let ptr = self.slot.swap(null_mut(), AcqRel);
        if ptr.is_null() {
            return None;
        }
        // Safety: the pointer came from a `UnparkTicket<'static, T>` and segments
        // are never freed while the lot lives, so the slot is still valid. The
        // swap made this clone the sole owner of the unpark right.
        UnparkTicket {
            slot: unsafe { &*ptr },
        }
        .unpark()
    }
}

impl<T> Clone for Waker<T> {
    fn clone(&self) -> Self {
        Self {
            slot: Arc::clone(&self.slot),
        }
    }
}

impl<T> Debug for Waker<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Waker").finish_non_exhaustive()
    }
}

/// A lock-free, single-occupancy holder for a [`ParkTicket`] ticket.
pub struct ParkTicketCell<T: 'static> {
    slot: AtomicPtr<Slot<T>>,
}

impl<T: Send> ParkTicketCell<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slot: AtomicPtr::new(null_mut()),
        }
    }

    /// Stores `ticket`. An occupied cell rejects the offer and hands
    /// `ticket` back, because dropping either ticket would leak its slot for
    /// the lifetime of the lot.
    pub fn put(&self, ticket: ParkTicket<'static, T>) -> Result<(), ParkTicket<'static, T>> {
        let ptr = from_ref(ticket.slot).cast_mut();
        match self.slot.compare_exchange(null_mut(), ptr, AcqRel, Relaxed) {
            Ok(_) => Ok(()),
            Err(_) => Err(ticket),
        }
    }

    /// Stores the reservation's park right and hands back the unpark
    /// ticket for the wake source. An occupied cell rejects the offer and
    /// returns the reservation whole, so the caller can release it instead
    /// of leaking the slot.
    pub fn put_reservation(
        &self,
        reservation: Reservation<'static, T>,
    ) -> Result<UnparkTicket<'static, T>, Reservation<'static, T>> {
        let ptr = from_ref(reservation.slot).cast_mut();
        match self.slot.compare_exchange(null_mut(), ptr, AcqRel, Relaxed) {
            Ok(_) => Ok(UnparkTicket {
                slot: reservation.slot,
            }),
            Err(_) => Err(reservation),
        }
    }

    /// Takes the held ticket, leaving the cell empty.
    #[must_use]
    pub fn take(&self) -> Option<ParkTicket<'static, T>> {
        let ptr = self.slot.swap(null_mut(), AcqRel);
        if ptr.is_null() {
            return None;
        }
        // Safety: the pointer was stored by `put` from a `ParkTicket<'static, T>`
        // and segments are never freed while the lot lives, so the slot stays
        // valid. The swap transferred sole ownership of the park right.
        Some(ParkTicket {
            slot: unsafe { &*ptr },
        })
    }

    #[must_use]
    pub fn has_ticket(&self) -> bool {
        !self.slot.load(Relaxed).is_null()
    }
}

impl<T: Send> Default for ParkTicketCell<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Debug for ParkTicketCell<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ParkTicketCell")
            .field("occupied", &!self.slot.load(Relaxed).is_null())
            .finish_non_exhaustive()
    }
}

/// Why a value could not be parked. Always carries the value back.
#[derive(Error)]
pub enum ParkError<T> {
    #[error("the slot was unparked before a value was parked")]
    Unparked(T),
}

impl<T> ParkError<T> {
    #[must_use]
    pub fn into_inner(self) -> T {
        match self {
            Self::Unparked(value) => value,
        }
    }
}

/// Written by hand rather than derived, so the payload needs no `Debug` bound
/// of its own and a whole parked value never lands in an error message.
impl<T> Debug for ParkError<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unparked(_) => f.debug_tuple("Unparked").finish_non_exhaustive(),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::Cell;
    use core::sync::atomic::AtomicUsize;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::{Arc, Barrier};
    use std::thread::{self, LocalKey};
    use std::thread_local;
    use std::vec::Vec;

    use super::*;

    thread_local! {
        /// The harness runs tests concurrently on separate threads, so a
        /// process wide counter would attribute another test's allocations to
        /// the window being measured.
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
        static DEALLOCS: Cell<usize> = const { Cell::new(0) };
    }

    /// Counts only and delegates every operation to the system allocator, so
    /// installing it process wide cannot change the behaviour of any test.
    struct Counting;

    impl Counting {
        /// A const initialized `Cell` needs neither a lazy allocation nor a
        /// destructor registration, so counting cannot re-enter the allocator.
        /// The access still fails during thread teardown, where the count no
        /// longer has a reader.
        fn bump(counter: &'static LocalKey<Cell<usize>>) {
            let _ = counter.try_with(|count| count.set(count.get() + 1));
        }
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            Self::bump(&ALLOCS);
            // Safety: the layout is forwarded unchanged from the caller.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            Self::bump(&DEALLOCS);
            // Safety: pointer and layout are forwarded unchanged, and every
            // live pointer came from `System` through `alloc`.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    fn allocs() -> usize {
        ALLOCS.with(Cell::get)
    }

    fn deallocs() -> usize {
        DEALLOCS.with(Cell::get)
    }

    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Relaxed);
        }
    }

    #[test]
    fn round_trip() {
        let lot = ParkingLot::new();
        let (park_ticket, unpark_ticket) = lot.reserve().split();
        assert!(
            park_ticket.park(7u32).is_ok(),
            "parking a fresh reservation failed"
        );
        assert_eq!(
            unpark_ticket.unpark(),
            Some(7),
            "the unpark returned the wrong value"
        );
    }

    #[test]
    fn unpark_before_park() {
        let lot = ParkingLot::new();
        let (park_ticket, unpark_ticket) = lot.reserve().split();

        assert_eq!(
            unpark_ticket.unpark(),
            None,
            "the unpark saw a value that was never parked"
        );

        let error = park_ticket
            .park(42u32)
            .expect_err("parking succeeded after the slot was unparked");
        assert_eq!(
            error.into_inner(),
            42,
            "the error did not carry the original value back"
        );
    }

    #[test]
    fn slot_reuse() {
        let lot = ParkingLot::new();
        for round in 0..4 {
            let tickets = (0..SEGMENT_SLOTS)
                .map(|_| lot.reserve().split())
                .collect::<Vec<_>>();
            for (i, (park_ticket, unpark_ticket)) in tickets.into_iter().enumerate() {
                assert!(
                    park_ticket.park(i).is_ok(),
                    "round {round} slot {i} refused the park"
                );
                assert_eq!(
                    unpark_ticket.unpark(),
                    Some(i),
                    "round {round} slot {i} lost its value"
                );
            }
        }
    }

    #[test]
    fn growth_across_segments() {
        let count = SEGMENT_SLOTS * 2 + 3;
        let lot = ParkingLot::new();
        let tickets = (0..count)
            .map(|_| lot.reserve().split())
            .collect::<Vec<_>>();

        let unpark_tickets = tickets
            .into_iter()
            .enumerate()
            .map(|(i, (park_ticket, unpark_ticket))| {
                assert!(park_ticket.park(i).is_ok(), "slot {i} refused the park");
                unpark_ticket
            })
            .collect::<Vec<_>>();

        for (i, unpark_ticket) in unpark_tickets.into_iter().enumerate() {
            assert_eq!(
                unpark_ticket.unpark(),
                Some(i),
                "slot {i} returned the wrong value"
            );
        }
    }

    #[test]
    fn distinct_slots() {
        let lot = ParkingLot::new();
        let (first_park, first_unpark) = lot.reserve().split();
        let (second_park, second_unpark) = lot.reserve().split();

        assert!(
            first_park.park(1u32).is_ok(),
            "first reservation refused the park"
        );
        assert!(
            second_park.park(2u32).is_ok(),
            "second reservation refused the park"
        );

        assert_eq!(
            first_unpark.unpark(),
            Some(1),
            "first reservation aliased the second"
        );
        assert_eq!(
            second_unpark.unpark(),
            Some(2),
            "second reservation aliased the first"
        );
    }

    #[test]
    fn drop_releases_parked_values() {
        let parked = Arc::new(AtomicUsize::new(0));
        let taken = Arc::new(AtomicUsize::new(0));
        let reserved = Arc::new(AtomicUsize::new(0));

        {
            let lot = ParkingLot::new();

            for _ in 0..3 {
                let (park_ticket, _keep) = lot.reserve().split();
                assert!(
                    park_ticket.park(DropCounter(Arc::clone(&parked))).is_ok(),
                    "parking a value failed"
                );
            }

            let (park_ticket, unpark_ticket) = lot.reserve().split();
            assert!(
                park_ticket.park(DropCounter(Arc::clone(&taken))).is_ok(),
                "parking the value to be retrieved failed"
            );
            drop(
                unpark_ticket
                    .unpark()
                    .expect("the parked value was not returned"),
            );
            assert_eq!(
                taken.load(Relaxed),
                1,
                "the retrieved value was not dropped by its owner"
            );

            let _reserved = lot.reserve();

            let (park_ticket, unpark_ticket) = lot.reserve().split();
            assert!(
                unpark_ticket.unpark().is_none(),
                "the unpark saw a value that was never parked"
            );
            let error = park_ticket
                .park(DropCounter(Arc::clone(&reserved)))
                .expect_err("parking succeeded on an unparked slot");
            drop(error.into_inner());
            assert_eq!(
                reserved.load(Relaxed),
                1,
                "the returned value was not dropped by its owner"
            );

            assert_eq!(
                parked.load(Relaxed),
                0,
                "a parked value was dropped before the lot"
            );
        }

        assert_eq!(
            parked.load(Relaxed),
            3,
            "the lot did not drop every parked value exactly once"
        );
        assert_eq!(
            taken.load(Relaxed),
            1,
            "a retrieved value was dropped twice"
        );
        assert_eq!(
            reserved.load(Relaxed),
            1,
            "a returned value was dropped twice"
        );
    }

    /// Snapshots the counters around the round trip alone, so a caller may let
    /// `reserve` append a segment first and still hold the round trip to zero.
    #[track_caller]
    fn assert_round_trip_does_not_allocate(
        park_ticket: ParkTicket<'_, usize>,
        unpark_ticket: UnparkTicket<'_, usize>,
        value: usize,
        label: &str,
    ) {
        let before_allocs = allocs();
        let before_deallocs = deallocs();

        assert!(park_ticket.park(value).is_ok(), "{label} refused the park");
        assert_eq!(
            unpark_ticket.unpark(),
            Some(value),
            "{label} returned the wrong value"
        );

        assert_eq!(allocs(), before_allocs, "{label} allocated");
        assert_eq!(deallocs(), before_deallocs, "{label} deallocated");
    }

    #[test]
    fn park_and_unpark_do_not_allocate() {
        let lot = ParkingLot::new();
        let (park_ticket, unpark_ticket) = lot.reserve().split();
        assert_round_trip_does_not_allocate(park_ticket, unpark_ticket, 9, "the head segment");
    }

    /// Holding whole segments worth of tickets puts the slot under test in a
    /// segment that was appended at run time rather than in the inline head,
    /// which is the case that would regress if a ticket ever went back to
    /// carrying an index that the park has to resolve.
    #[test]
    fn park_does_not_allocate_in_appended_segment() {
        const LABELS: [&str; 2] = ["the first appended segment", "the second appended segment"];

        let lot = ParkingLot::new();
        let mut held = Vec::with_capacity(SEGMENT_SLOTS * LABELS.len());

        for (round, label) in LABELS.into_iter().enumerate() {
            for _ in 0..SEGMENT_SLOTS {
                held.push(lot.reserve());
            }
            let (park_ticket, unpark_ticket) = lot.reserve().split();
            assert_round_trip_does_not_allocate(park_ticket, unpark_ticket, round, label);
        }
    }

    #[test]
    fn reserve_allocates_only_when_appending_a_segment() {
        let lot = ParkingLot::new();
        // Sized up front, because a push that grew the vector inside the
        // window below would be counted against `reserve`.
        let mut held = Vec::with_capacity(SEGMENT_SLOTS);

        let before = allocs();
        for _ in 0..SEGMENT_SLOTS {
            held.push(lot.reserve());
        }
        assert_eq!(
            allocs(),
            before,
            "reserving a free slot of an existing segment allocated"
        );

        let before = allocs();
        let (park_ticket, unpark_ticket) = lot.reserve().split();
        assert_eq!(
            allocs() - before,
            1,
            "appending a segment took more than one allocation"
        );

        assert_round_trip_does_not_allocate(
            park_ticket,
            unpark_ticket,
            1,
            "a freshly appended segment",
        );
    }

    #[test]
    fn race_park_against_unpark() {
        const PAIRS: usize = 8;

        let lot = ParkingLot::new();
        for round in 0..4 {
            let tickets = (0..PAIRS)
                .map(|_| lot.reserve().split())
                .collect::<Vec<_>>();
            let barrier = Barrier::new(PAIRS * 2);

            thread::scope(|scope| {
                let barrier = &barrier;
                let handles = tickets
                    .into_iter()
                    .enumerate()
                    .map(|(i, (park_ticket, unpark_ticket))| {
                        let value = round * PAIRS + i;
                        let parker = scope.spawn(move || {
                            barrier.wait();
                            park_ticket.park(value)
                        });
                        let waker = scope.spawn(move || {
                            barrier.wait();
                            unpark_ticket.unpark()
                        });
                        (i, parker, waker)
                    })
                    .collect::<Vec<_>>();

                for (i, parker, waker) in handles {
                    let value = round * PAIRS + i;
                    let parked = parker.join().expect("the park thread panicked");
                    let taken = waker.join().expect("the unpark thread panicked");

                    match (parked, taken) {
                        (Ok(()), Some(got)) => {
                            assert_eq!(got, value, "pair {i} unparked the wrong value");
                        }
                        (Err(error), None) => assert_eq!(
                            error.into_inner(),
                            value,
                            "pair {i} returned the wrong value to the park side"
                        ),
                        (Ok(()), None) => panic!("pair {i} lost the value"),
                        (Err(_), Some(_)) => panic!("pair {i} duplicated the value"),
                    }
                }
            });
        }
    }

    #[test]
    fn try_reserve_never_appends() {
        let lot = ParkingLot::new();
        let mut held = Vec::with_capacity(SEGMENT_SLOTS + 1);
        for _ in 0..SEGMENT_SLOTS {
            held.push(
                lot.try_reserve()
                    .expect("try_reserve refused a free inline slot"),
            );
        }

        assert!(
            lot.try_reserve().is_none(),
            "try_reserve appended a segment instead of refusing"
        );

        held.push(lot.reserve());
        let (park_ticket, unpark_ticket) = lot
            .try_reserve()
            .expect("try_reserve missed a free slot in an appended segment")
            .split();
        assert!(
            park_ticket.park(5usize).is_ok(),
            "appended-segment slot refused the park"
        );
        assert_eq!(
            unpark_ticket.unpark(),
            Some(5),
            "appended-segment slot lost its value"
        );
    }

    #[test]
    fn waker_fires_once() {
        static LOT: ParkingLot<u32> = ParkingLot::new();
        let (park_ticket, unpark_ticket) = LOT.reserve().split();
        assert!(park_ticket.park(11).is_ok(), "parking failed");

        let waker = unpark_ticket.into_waker();
        let clone = waker.clone();
        assert_eq!(waker.wake(), Some(11), "first wake lost the value");
        assert_eq!(clone.wake(), None, "second clone fired again");
        assert_eq!(waker.wake(), None, "the original fired again");
    }

    #[test]
    fn wake_before_park() {
        static LOT: ParkingLot<u32> = ParkingLot::new();
        let (park_ticket, unpark_ticket) = LOT.reserve().split();

        let waker = unpark_ticket.into_waker();
        assert_eq!(waker.wake(), None, "wake saw a value that was never parked");

        let error = park_ticket
            .park(23)
            .expect_err("parking succeeded after the waker fired");
        assert_eq!(
            error.into_inner(),
            23,
            "the error did not carry the original value back"
        );
    }

    #[test]
    fn racing_waker_clones_produce_one_value() {
        static LOT: ParkingLot<usize> = ParkingLot::new();
        for round in 0..64 {
            let (park_ticket, unpark_ticket) = LOT.reserve().split();
            assert!(
                park_ticket.park(round).is_ok(),
                "round {round} parking failed"
            );

            let waker = unpark_ticket.into_waker();
            let barrier = Barrier::new(2);
            let (first, second) = thread::scope(|scope| {
                let barrier = &barrier;
                let first = {
                    let waker = waker.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        waker.wake()
                    })
                };
                let second = scope.spawn(move || {
                    barrier.wait();
                    waker.wake()
                });
                (
                    first.join().expect("first waker thread panicked"),
                    second.join().expect("second waker thread panicked"),
                )
            });

            match (first, second) {
                (Some(got), None) | (None, Some(got)) => {
                    assert_eq!(got, round, "round {round} woke the wrong value");
                }
                (Some(_), Some(_)) => panic!("round {round} duplicated the value"),
                (None, None) => panic!("round {round} lost the value"),
            }
        }
    }

    #[test]
    fn cell_round_trip() {
        static LOT: ParkingLot<u32> = ParkingLot::new();
        let cell = ParkTicketCell::new();
        assert!(cell.take().is_none(), "an empty cell produced a ticket");
        assert!(!cell.has_ticket(), "an empty cell claims to hold a ticket");

        let (park_ticket, unpark_ticket) = LOT.reserve().split();
        assert!(
            cell.put(park_ticket).is_ok(),
            "filling an empty cell failed"
        );
        assert!(cell.has_ticket(), "an occupied cell claims to be empty");

        let taken = cell.take().expect("taking from an occupied cell failed");
        assert!(!cell.has_ticket(), "the cell stayed occupied after take");
        assert!(cell.take().is_none(), "a second take produced a ticket");

        assert!(taken.park(7).is_ok(), "the ticket died in the cell");
        assert_eq!(
            unpark_ticket.unpark(),
            Some(7),
            "the value did not survive the cell round trip"
        );
    }

    #[test]
    fn cell_rejects_second_put() {
        static LOT: ParkingLot<u32> = ParkingLot::new();
        let cell = ParkTicketCell::new();
        let (first_park, first_unpark) = LOT.reserve().split();
        let (second_park, second_unpark) = LOT.reserve().split();

        assert!(cell.put(first_park).is_ok(), "filling an empty cell failed");
        let rejected = cell
            .put(second_park)
            .expect_err("an occupied cell accepted a second ticket");

        // The rejected ticket must still be the second reservation, not a
        // swapped-out first one.
        assert!(rejected.park(2).is_ok(), "the rejected ticket died");
        assert_eq!(
            second_unpark.unpark(),
            Some(2),
            "the rejected ticket lost its slot identity"
        );

        let kept = cell.take().expect("the held ticket vanished");
        assert!(kept.park(1).is_ok(), "the kept ticket died");
        assert_eq!(
            first_unpark.unpark(),
            Some(1),
            "the kept ticket lost its slot identity"
        );
    }

    #[test]
    fn release_returns_slot_to_lot() {
        let lot = ParkingLot::<u32>::new();
        let mut held = Vec::with_capacity(SEGMENT_SLOTS);
        for _ in 0..SEGMENT_SLOTS {
            held.push(
                lot.try_reserve()
                    .expect("try_reserve refused a free inline slot"),
            );
        }
        assert!(
            lot.try_reserve().is_none(),
            "a full lot handed out a reservation"
        );

        held.pop().expect("held reservations vanished").release();
        let reservation = lot.try_reserve().expect("a released slot was not reusable");
        reservation.release();
    }

    #[test]
    fn cell_put_reservation_round_trip() {
        static LOT: ParkingLot<u32> = ParkingLot::new();
        let cell = ParkTicketCell::new();

        let unpark_ticket = cell
            .put_reservation(LOT.reserve())
            .expect("filling an empty cell rejected the reservation");
        let park_ticket = cell.take().expect("the held ticket vanished");
        assert!(park_ticket.park(3).is_ok(), "the ticket died in the cell");
        assert_eq!(
            unpark_ticket.unpark(),
            Some(3),
            "the unpark ticket did not belong to the stored reservation"
        );
    }

    #[test]
    fn cell_put_reservation_rejects_when_occupied() {
        static LOT: ParkingLot<u32> = ParkingLot::new();
        let cell = ParkTicketCell::new();

        let _unpark_ticket = cell
            .put_reservation(LOT.reserve())
            .expect("filling an empty cell rejected the reservation");
        let rejected = cell
            .put_reservation(LOT.reserve())
            .expect_err("an occupied cell accepted a second reservation");
        rejected.release();
        assert!(cell.has_ticket(), "the rejection emptied the cell");
    }
}
