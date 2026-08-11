use alloc::boxed::Box;
use alloc::vec::Vec;
use core::pin::Pin;

use kernel_park::{ParkTicket, ParkingLot, Reservation, UnparkTicket, Waker};
use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::hpet::hpet_maybe;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::Task;

static WAITING: ParkingLot<Pin<Box<Task>>> = ParkingLot::new();

pub type TaskReservation = Reservation<'static, Pin<Box<Task>>>;
pub type TaskParkTicket = ParkTicket<'static, Pin<Box<Task>>>;
pub type TaskUnparkTicket = UnparkTicket<'static, Pin<Box<Task>>>;
pub type TaskWaker = Waker<Pin<Box<Task>>>;

/// Task context only. May allocate a fresh segment.
pub fn reserve() -> TaskReservation {
    WAITING.reserve()
}

/// Never allocates, so this is the only reservation legal in interrupt
/// context. `None` means every existing slot is taken.
pub fn try_reserve() -> Option<TaskReservation> {
    WAITING.try_reserve()
}

pub fn wake(waker: &TaskWaker) {
    if let Some(task) = waker.wake() {
        GlobalTaskQueue::enqueue(task);
    }
}

pub fn unpark_and_enqueue(ticket: TaskUnparkTicket) {
    if let Some(task) = ticket.unpark() {
        GlobalTaskQueue::enqueue(task);
    }
}

/// Parks the current task until a holder of the matching unpark ticket
/// wakes it.
///
/// # Panics
/// Panics when the task already holds a park ticket. Only a kernel bug can
/// park one task twice.
pub fn block_current(ticket: TaskParkTicket) {
    let ctx = ExecutionContext::load();
    let parked = ctx.current_task().set_park_ticket(ticket);
    assert!(
        parked.is_ok(),
        "the blocking task already holds a park ticket"
    );
    interrupts::disable();
    unsafe {
        ctx.scheduler_mut().reschedule();
    }
    interrupts::enable();
}

struct Sleeper {
    deadline_ns: u64,
    waker: TaskWaker,
}

static SLEEPERS: Mutex<Vec<Sleeper>> = Mutex::new(Vec::new());

/// Task context, interrupts may be enabled.
pub fn sleep_until(deadline_ns: u64, waker: TaskWaker) {
    SLEEPERS.lock().push(Sleeper { deadline_ns, waker });
}

/// Wakes every sleeper whose deadline has passed.
pub fn wake_expired_sleepers() {
    let Some(mut sleepers) = SLEEPERS.try_lock() else {
        return;
    };
    let Some(hpet) = hpet_maybe() else {
        return;
    };
    let now = hpet.read().elapsed_ns();
    let mut i = 0;
    while i < sleepers.len() {
        if sleepers[i].deadline_ns <= now {
            let sleeper = sleepers.swap_remove(i);
            wake(&sleeper.waker);
        } else {
            i += 1;
        }
    }
}
