use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::ffi::c_void;
use core::pin::Pin;
use core::ptr::NonNull;
use core::sync::atomic::Ordering::Relaxed;
use core::sync::atomic::{AtomicBool, AtomicU64};

use cordyceps::Linked;
use cordyceps::mpsc_queue::Links;
use kernel_park::ParkTicketCell;
use spin::RwLock;
use tracing::trace;
use x86_64::instructions::{hlt, interrupts};

use crate::U64Ext;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::wait::{TaskParkTicket, TaskReservation, TaskUnparkTicket};
use crate::mem::memapi::{LowerHalfAllocation, Writable};

mod id;
pub use id::*;
mod queue;
pub use queue::*;
mod stack;
pub use stack::*;
mod state;
pub use state::*;

#[derive(Debug)]
pub struct Task {
    /// The unique identifier of the task.
    tid: TaskId,
    /// The name of the task, not necessarily unique.
    name: String,
    /// The parent process that this task belongs to.
    /// If upon rescheduling, the parent process is not alive, the task will be terminated.
    process: Arc<Process>,
    /// Whether this task should be terminated upon the next reschedule.
    /// This can be set at any point.
    should_terminate: AtomicBool,
    /// Set by the task itself before it blocks or by tick signal delivery on
    /// this task's own Ring 3 frame. Consumed by the next reschedule on the
    /// CPU running this task.
    ///
    /// The parked value is always this task's own `Pin<Box<Task>>`. The
    /// scheduler takes the ticket from the task it switches away from and
    /// parks that same box, so an unpark from a wake source always
    /// yields the task that carried the ticket.
    park_ticket: ParkTicketCell<Pin<Box<Task>>>,
    pending_fault_addr: AtomicU64,
    /// The stack pointer of the task at the time of the last context switch.
    /// If this task is currently running, then this value is not the current stack pointer.
    /// This must be set during the context switch.
    last_stack_ptr: Pin<Box<usize>>,
    state: State,
    /// The kernel stack of the task. Every task starts with a stack in the higher half.
    /// Userspace tasks will then allocate a stack in the lower half, which will be stored in
    /// `ustack`.
    kstack: Option<HigherHalfStack>,

    /// The user stack of the task. This is only set if the task is a userspace task.
    ustack: RwLock<Option<LowerHalfAllocation<Writable>>>,
    tls: RwLock<Option<LowerHalfAllocation<Writable>>>,
    fx_area: RwLock<Option<LowerHalfAllocation<Writable>>>,

    links: Links<Self>,
}

#[repr(C, align(16))]
pub(crate) struct FxArea {
    data: [u8; 512],
}

impl Unpin for Task {}

unsafe impl Linked<Links<Self>> for Task {
    type Handle = Pin<Box<Self>>;

    fn into_ptr(r: Self::Handle) -> NonNull<Self> {
        NonNull::from(Box::leak(Pin::into_inner(r)))
    }

    unsafe fn from_ptr(ptr: NonNull<Self>) -> Self::Handle {
        unsafe { Pin::new(Box::from_raw(ptr.as_ptr())) }
    }

    unsafe fn links(ptr: NonNull<Self>) -> NonNull<Links<Self>> {
        let links = unsafe { &raw mut (*ptr.as_ptr()).links };
        unsafe { NonNull::new_unchecked(links) }
    }
}

impl Task {
    /// Creates a new stack in the specified process. Stack will be allocated immediately in the
    /// current address space.
    ///
    /// # Errors
    /// Returns an error if the stack could not be allocated.
    pub fn create_new(
        process: &Arc<Process>,
        entry_point: extern "C" fn(*mut c_void),
        arg: *mut c_void,
    ) -> Result<Self, StackAllocationError> {
        let stack = HigherHalfStack::allocate(16, entry_point, arg, Self::exit)?;
        Ok(Self::create_with_stack(process, stack))
    }

    pub fn create_with_stack(process: &Arc<Process>, stack: HigherHalfStack) -> Self {
        let tid = TaskId::new();
        let name = format!("task-{tid}");
        let process = process.clone();
        let should_terminate = AtomicBool::new(false);
        let state = State::Ready;
        let last_stack_ptr = Box::pin(stack.initial_rsp().as_u64().into_usize());
        let links = Links::default();
        Self {
            tid,
            name,
            process,
            should_terminate,
            park_ticket: ParkTicketCell::new(),
            pending_fault_addr: AtomicU64::new(0),
            last_stack_ptr,
            state,
            kstack: Some(stack),
            ustack: RwLock::new(None),
            tls: RwLock::new(None),
            fx_area: RwLock::new(None),
            links,
        }
    }

    pub(in crate::mcore::mtask) fn create_stub() -> Self {
        let tid = TaskId::new();
        let name = "stub".to_string();
        let process = Process::root().clone();
        let should_terminate = AtomicBool::new(false);
        let last_stack_ptr = Box::pin(0);
        let state = State::Finished;
        let links = Links::new_stub();
        Self {
            tid,
            name,
            process,
            should_terminate,
            park_ticket: ParkTicketCell::new(),
            pending_fault_addr: AtomicU64::new(0),
            last_stack_ptr,
            state,
            kstack: None,
            ustack: RwLock::new(None),
            tls: RwLock::new(None),
            fx_area: RwLock::new(None),
            links,
        }
    }

    pub(crate) extern "C" fn exit() {
        Self::exit_current()
    }

    pub(crate) fn exit_current() -> ! {
        let ctx = ExecutionContext::load();
        let task = ctx.current_task();
        trace!(name = %task.name(), "exiting task");

        unsafe {
            task.ustack.force_write_unlock();
            task.tls.force_write_unlock();
            task.fx_area.force_write_unlock();

            let _ = task.fx_area.write().take();
            let _ = task.tls.write().take();
            let _ = task.ustack.write().take();

            task.set_should_terminate(true);
        }

        // A terminated task is disposed without saving its context, so the
        // current stack is simply abandoned. That covers the page fault IST
        // stack on the fault termination path.
        interrupts::disable();
        loop {
            unsafe {
                ctx.scheduler_mut().reschedule();
            }
            interrupts::enable();
            hlt();
            interrupts::disable();
        }
    }

    /// Creates a Task struct for the current state of the CPU.
    /// The task is inactive, and its values must be set by the scheduler
    /// first.
    ///
    /// The resulting task will belong to the root process.
    ///
    /// # Safety
    /// The caller must ensure that this is only called once per core.
    #[must_use]
    pub unsafe fn create_current() -> Self {
        let tid = TaskId::new();
        let name = format!("task-{tid}");
        let process = Process::root().clone();
        let should_terminate = AtomicBool::new(false);
        let last_stack_ptr = Box::pin(0);
        let state = State::Running;
        Self {
            tid,
            name,
            process,
            should_terminate,
            park_ticket: ParkTicketCell::new(),
            pending_fault_addr: AtomicU64::new(0),
            last_stack_ptr,
            state,
            kstack: None,
            ustack: RwLock::new(None),
            tls: RwLock::new(None),
            fx_area: RwLock::new(None),
            links: Links::default(),
        }
    }

    pub fn id(&self) -> TaskId {
        self.tid
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn process(&self) -> &Arc<Process> {
        &self.process
    }

    pub fn should_terminate(&self) -> bool {
        self.should_terminate.load(Relaxed)
    }

    pub fn set_should_terminate(&self, should_terminate: bool) {
        self.should_terminate.store(should_terminate, Relaxed);
    }

    /// Leaves the ticket the next reschedule parks this task with.
    ///
    /// A held ticket is never overwritten, because dropping a `ParkTicket`
    /// leaks its slot for the lifetime of the lot. The offered ticket comes
    /// back as `Err` in that case, which requires a kernel bug to reach.
    pub fn set_park_ticket(&self, ticket: TaskParkTicket) -> Result<(), TaskParkTicket> {
        self.park_ticket.put(ticket)
    }

    /// Like [`Task::set_park_ticket`], but from an unsplit reservation, so a
    /// rejection returns the reservation for release instead of leaking it.
    /// Success hands back the unpark ticket for the wake source.
    pub(crate) fn set_park_reservation(
        &self,
        reservation: TaskReservation,
    ) -> Result<TaskUnparkTicket, TaskReservation> {
        self.park_ticket.put_reservation(reservation)
    }

    pub(in crate::mcore::mtask) fn has_park_ticket(&self) -> bool {
        self.park_ticket.has_ticket()
    }

    pub(in crate::mcore::mtask) fn take_park_ticket(&self) -> Option<TaskParkTicket> {
        self.park_ticket.take()
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn kstack(&self) -> &Option<HigherHalfStack> {
        &self.kstack
    }

    pub fn pending_fault_addr(&self) -> &AtomicU64 {
        &self.pending_fault_addr
    }

    pub fn ustack(&self) -> &RwLock<Option<LowerHalfAllocation<Writable>>> {
        &self.ustack
    }

    pub fn tls(&self) -> &RwLock<Option<LowerHalfAllocation<Writable>>> {
        &self.tls
    }

    pub fn fx_area(&self) -> &RwLock<Option<LowerHalfAllocation<Writable>>> {
        &self.fx_area
    }

    pub fn last_stack_ptr(&mut self) -> &mut usize {
        self.last_stack_ptr.as_mut().get_mut()
    }
}
