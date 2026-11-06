use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::ffi::c_void;
use core::pin::Pin;
use core::ptr::NonNull;
use core::sync::atomic::Ordering::Relaxed;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};

use cordyceps::Linked;
use cordyceps::mpsc_queue::Links;
use kernel_abi::SigSet;
use kernel_log::SpanStack;
use kernel_park::ParkTicketCell;
use kernel_syscall::signal::SignalMask;
use spin::{Mutex, RwLock, RwLockWriteGuard};
use tracing::trace;
use x86_64::VirtAddr;
use x86_64::instructions::{hlt, interrupts};

use crate::U64Ext;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::process::tls::TlsBlock;
use crate::mcore::mtask::wait::{TaskParkTicket, TaskReservation, TaskUnparkTicket};
use crate::mem::memapi::{LowerHalfAllocation, Writable};

mod id;
pub use id::*;
mod queue;
pub use queue::*;
mod stack;
pub use stack::*;

#[derive(Debug)]
pub struct Task {
    tid: TaskId,
    /// Not necessarily unique.
    name: String,
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
    /// The kernel stack of the task. Every task starts with a stack in the higher half.
    /// Userspace tasks will then allocate a stack in the lower half, which will be stored in
    /// `ustack`.
    kstack: Option<HigherHalfStack>,

    /// The user stack of the task. This is only set if the task is a userspace task.
    ustack: RwLock<Option<LowerHalfAllocation<Writable>>>,
    tls: RwLock<Option<TlsBlock>>,
    fx_area: RwLock<Option<LowerHalfAllocation<Writable>>>,
    /// This field holds the bits of the [`SignalMask`] of this thread. No two
    /// writers race (see [`Task::set_signal_mask`]). Therefore a plain load
    /// and store are enough.
    signal_mask: AtomicU64,
    /// This field counts the user memory guards that this task holds. A task
    /// with a guard must not park.
    user_memory_pins: AtomicU32,

    /// Only the CPU currently running this task ever touches this stack, so the
    /// lock is never contended. It lives on the task rather than per CPU because
    /// an entered span stays entered across a migration to another CPU.
    span_stack: Mutex<SpanStack>,

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
    /// The task currently running on this CPU.
    ///
    /// The reference stays valid after interrupts are re-enabled, because
    /// tasks are heap-pinned and a task cannot be freed while its own code
    /// is still running.
    ///
    /// # Panics
    /// Panics before the CPU's execution context is installed.
    #[must_use]
    pub fn current() -> &'static Task {
        Self::try_current().expect("no execution context on this cpu")
    }

    /// Like [`Task::current`], but `None` before the CPU's execution context
    /// is installed.
    #[must_use]
    pub fn try_current() -> Option<&'static Task> {
        interrupts::without_interrupts(|| {
            // Safety: interrupts are off, so this cannot migrate between
            // loading the CPU-local context and reading its current task.
            Some(unsafe { ExecutionContext::try_load() }?.current_task())
        })
    }

    /// Creates a task with a freshly allocated kernel stack that starts in
    /// `entry_point` and terminates the task when `entry_point` returns.
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
        process.register_task();
        let should_terminate = AtomicBool::new(false);
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
            kstack: Some(stack),
            ustack: RwLock::new(None),
            tls: RwLock::new(None),
            fx_area: RwLock::new(None),
            signal_mask: AtomicU64::new(0),
            user_memory_pins: AtomicU32::new(0),
            span_stack: Mutex::new(SpanStack::new()),
            links,
        }
    }

    pub(in crate::mcore::mtask) fn create_stub() -> Self {
        let tid = TaskId::new();
        let name = "stub".to_string();
        let process = Process::root().clone();
        process.register_task();
        let should_terminate = AtomicBool::new(false);
        let last_stack_ptr = Box::pin(0);
        let links = Links::new_stub();
        Self {
            tid,
            name,
            process,
            should_terminate,
            park_ticket: ParkTicketCell::new(),
            pending_fault_addr: AtomicU64::new(0),
            last_stack_ptr,
            kstack: None,
            ustack: RwLock::new(None),
            tls: RwLock::new(None),
            fx_area: RwLock::new(None),
            signal_mask: AtomicU64::new(0),
            user_memory_pins: AtomicU32::new(0),
            span_stack: Mutex::new(SpanStack::new()),
            links,
        }
    }

    /// This function drops the user stack, TLS and FX area, and unmaps their
    /// lower-half pages. `_exclusive` proves that no task of the process
    /// copies from or to user memory during this time.
    ///
    /// # Panics
    /// This function panics if interrupts are off. In debug builds, it also
    /// panics if the address space of the owning process is not active on
    /// this CPU.
    pub fn free_user_allocations(&self, _exclusive: &RwLockWriteGuard<'_, ()>) {
        debug_assert!(self.process.address_space().is_active());
        let _ = self.fx_area.write().take();
        let _ = self.tls.write().take();
        let _ = self.ustack.write().take();
    }

    pub fn per_thread_block_starts(&self) -> impl Iterator<Item = VirtAddr> {
        [
            self.ustack.read().as_ref().map(|stack| stack.start()),
            self.tls.read().as_ref().map(|tls| tls.start()),
            self.fx_area.read().as_ref().map(|fx| fx.start()),
        ]
        .into_iter()
        .flatten()
    }

    /// This function drops the handles of the user stack, TLS and FX area,
    /// and keeps their pages mapped. Use it for a task that ends with
    /// interrupts off. Such a task ends only when its whole process ends or
    /// when an exec or exit reaps it. Exec or the end of the process then
    /// removes the regions.
    pub fn abandon_user_allocations(&self) {
        if let Some(fx_area) = self.fx_area.write().take() {
            fx_area.abandon();
        }
        if let Some(tls) = self.tls.write().take() {
            tls.abandon();
        }
        if let Some(ustack) = self.ustack.write().take() {
            ustack.abandon();
        }
    }

    pub(crate) extern "C" fn exit() {
        Self::exit_current()
    }

    pub(crate) fn exit_current() -> ! {
        let task = Task::current();
        trace!(name = %task.name(), "exiting task");

        unsafe {
            task.ustack.force_write_unlock();
            task.tls.force_write_unlock();
            task.fx_area.force_write_unlock();
        }
        if interrupts::are_enabled() {
            let process = task.process();
            task.free_user_allocations(&process.exclusive_user_memory());
        } else {
            task.abandon_user_allocations();
        }
        task.set_should_terminate(true);

        // A terminated task is disposed without saving its context, so the
        // current stack is simply abandoned. That covers the page fault IST
        // stack on the fault termination path.
        interrupts::disable();
        loop {
            unsafe {
                // Safety: interrupts are off, so the context is this CPU's
                // and the reschedule cannot be preempted.
                ExecutionContext::load().scheduler_mut().reschedule();
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
        process.register_task();
        let should_terminate = AtomicBool::new(false);
        let last_stack_ptr = Box::pin(0);
        Self {
            tid,
            name,
            process,
            should_terminate,
            park_ticket: ParkTicketCell::new(),
            pending_fault_addr: AtomicU64::new(0),
            last_stack_ptr,
            kstack: None,
            ustack: RwLock::new(None),
            tls: RwLock::new(None),
            fx_area: RwLock::new(None),
            signal_mask: AtomicU64::new(0),
            user_memory_pins: AtomicU32::new(0),
            span_stack: Mutex::new(SpanStack::new()),
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

    pub fn kstack(&self) -> &Option<HigherHalfStack> {
        &self.kstack
    }

    pub fn pending_fault_addr(&self) -> &AtomicU64 {
        &self.pending_fault_addr
    }

    pub(crate) fn user_memory_pins(&self) -> &AtomicU32 {
        &self.user_memory_pins
    }

    pub(crate) fn span_stack(&self) -> &Mutex<SpanStack> {
        &self.span_stack
    }

    pub fn ustack(&self) -> &RwLock<Option<LowerHalfAllocation<Writable>>> {
        &self.ustack
    }

    pub fn tls(&self) -> &RwLock<Option<TlsBlock>> {
        &self.tls
    }

    pub fn fx_area(&self) -> &RwLock<Option<LowerHalfAllocation<Writable>>> {
        &self.fx_area
    }

    pub fn signal_mask(&self) -> SignalMask {
        SignalMask::blocking(SigSet::from_bits(self.signal_mask.load(Relaxed)))
    }

    /// Only the task itself may change the mask. The creator of a task that
    /// has not run yet may also change it.
    pub fn set_signal_mask(&self, mask: SignalMask) {
        self.signal_mask.store(mask.blocked().bits(), Relaxed);
    }

    /// This function returns true when a pending signal that this task does
    /// not block must end an interruptible wait of the task.
    pub fn has_interrupting_signal(&self) -> bool {
        self.process
            .signals_read()
            .has_interrupting_deliverable(self.signal_mask())
    }

    pub fn last_stack_ptr(&mut self) -> &mut usize {
        self.last_stack_ptr.as_mut().get_mut()
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.process.retire_task();
    }
}
