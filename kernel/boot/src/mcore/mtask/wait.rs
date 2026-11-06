use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::num::NonZeroU64;
use core::pin::Pin;
use core::task::Waker;

use kernel_park::{ParkTicket, ParkingLot, Reservation, UnparkTicket};
use kernel_vfs::{WaitKey, WaitSource};
use x86_64::instructions::interrupts;

use crate::hpet::{Instant, hpet_maybe};
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::Task;
use crate::sync::IrqMutex;

static WAITING: ParkingLot<Pin<Box<Task>>> = ParkingLot::new();

pub type TaskReservation = Reservation<'static, Pin<Box<Task>>>;
pub type TaskParkTicket = ParkTicket<'static, Pin<Box<Task>>>;
pub type TaskUnparkTicket = UnparkTicket<'static, Pin<Box<Task>>>;

/// Task context only. May allocate a fresh segment.
pub fn reserve() -> TaskReservation {
    WAITING.reserve()
}

/// Never allocates, so this is the only reservation legal in interrupt
/// context. `None` means every existing slot is taken.
pub fn try_reserve() -> Option<TaskReservation> {
    WAITING.try_reserve()
}

pub fn unpark_and_enqueue(ticket: TaskUnparkTicket) {
    if let Some(task) = ticket.unpark() {
        GlobalTaskQueue::enqueue(task);
    }
}

/// Hands out identities that never repeat within one owner.
struct KeySequence(u64);

impl KeySequence {
    const fn new() -> Self {
        Self(0)
    }

    fn next(&mut self) -> NonZeroU64 {
        self.0 = self.0.checked_add(1).expect("wait identities exhausted");
        NonZeroU64::new(self.0).expect("a checked increment from zero is never zero")
    }
}

struct Registrations {
    keys: KeySequence,
    wakers: Vec<(WaitKey, Waker)>,
}

/// Holds the tasks that wait on one resource. `wake_all` is safe from any
/// context, including interrupt handlers.
pub struct WaitQueue(IrqMutex<Registrations>);

impl WaitQueue {
    pub const fn new() -> Self {
        Self(IrqMutex::new(Registrations {
            keys: KeySequence::new(),
            wakers: Vec::new(),
        }))
    }

    /// Takes every registration and wakes it. A waker of a task that no
    /// longer waits does nothing.
    pub fn wake_all(&self) {
        let wakers = core::mem::take(&mut self.0.lock_without_interrupts().wakers);
        for (_, waker) in wakers {
            waker.wake();
        }
    }
}

impl WaitSource for WaitQueue {
    fn register(&self, waker: Waker) -> WaitKey {
        let mut registrations = self.0.lock_without_interrupts();
        let key = WaitKey::new(registrations.keys.next());
        registrations.wakers.push((key, waker));
        key
    }

    fn unregister(&self, key: WaitKey) {
        let removed = {
            let mut registrations = self.0.lock_without_interrupts();
            registrations
                .wakers
                .iter()
                .position(|(k, _)| *k == key)
                .map(|i| registrations.wakers.swap_remove(i))
        };
        drop(removed);
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Parks the current task until a holder of the matching unpark ticket
/// wakes it.
///
/// # Panics
/// Panics when the task already holds a park ticket. Only a kernel bug can
/// park one task twice.
pub(in crate::mcore::mtask) fn block_current(ticket: TaskParkTicket) {
    let parked = Task::current().set_park_ticket(ticket);
    assert!(
        parked.is_ok(),
        "the blocking task already holds a park ticket"
    );
    interrupts::disable();
    unsafe {
        // Safety: interrupts are off, so the context is this CPU's and the
        // reschedule cannot be preempted.
        ExecutionContext::load().scheduler_mut().reschedule();
    }
    interrupts::enable();
}

/// Orders sleepers by deadline. The ID separates equal deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SleepKey {
    deadline: Instant,
    id: NonZeroU64,
}

struct Sleepers {
    ids: KeySequence,
    wakers: BTreeMap<SleepKey, Waker>,
}

static SLEEPERS: IrqMutex<Sleepers> = IrqMutex::new(Sleepers {
    ids: KeySequence::new(),
    wakers: BTreeMap::new(),
});

/// Removes its deadline registration on drop. The timer can remove it first.
#[must_use = "dropping the registration cancels the deadline"]
pub struct SleepRegistration {
    key: SleepKey,
}

impl Drop for SleepRegistration {
    fn drop(&mut self) {
        let removed = SLEEPERS.lock_without_interrupts().wakers.remove(&self.key);
        drop(removed);
    }
}

/// Task context, interrupts may be enabled.
pub fn sleep_until(deadline: Instant, waker: Waker) -> SleepRegistration {
    let mut sleepers = SLEEPERS.lock_without_interrupts();
    let key = SleepKey {
        deadline,
        id: sleepers.ids.next(),
    };
    sleepers.wakers.insert(key, waker);
    SleepRegistration { key }
}

/// Wakes every sleeper whose deadline has passed. The timer interrupt calls
/// this function.
pub fn wake_expired_sleepers() {
    let Some(hpet) = hpet_maybe() else {
        return;
    };
    let now = hpet.read().now();
    loop {
        let waker = {
            let mut sleepers = SLEEPERS.lock_without_interrupts();
            match sleepers.wakers.first_entry() {
                Some(earliest) if earliest.key().deadline <= now => earliest.remove(),
                _ => return,
            }
        };
        waker.wake();
    }
}
