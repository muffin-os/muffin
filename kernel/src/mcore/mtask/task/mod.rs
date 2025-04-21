use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::Process;
use crate::mem::virt::VirtualMemoryHigherHalf;
use crate::U64Ext;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use cordyceps::mpsc_queue::Links;
use cordyceps::Linked;
use core::ffi::c_void;
use core::pin::Pin;
use core::ptr::NonNull;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering::Relaxed;
pub use id::*;
use log::trace;
pub use queue::*;
pub use stack::*;
pub use state::*;
use x86_64::instructions::hlt;

mod id;
mod queue;
mod stack;
mod state;

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
    /// The stack pointer of the task at the time of the last context switch.
    /// If this task is currently running, then this value is not the current stack pointer.
    /// This must be set during the context switch.
    last_stack_ptr: Pin<Box<usize>>,
    state: State,
    stack: Option<Stack>,

    links: Links<Self>,
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
        let stack = Stack::allocate(16, VirtualMemoryHigherHalf, entry_point, arg, Self::exit)?;
        Ok(Self::create_with_stack(process, stack))
    }

    pub fn create_with_stack(process: &Arc<Process>, stack: Stack) -> Self {
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
            last_stack_ptr,
            state,
            stack: Some(stack),
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
        let stack = None;
        let links = Links::new_stub();
        Self {
            tid,
            name,
            process,
            should_terminate,
            last_stack_ptr,
            state,
            stack,
            links,
        }
    }

    pub(crate) extern "C" fn exit() {
        let task = ExecutionContext::load().current_task();
        trace!("exiting task {}", task.name());

        task.set_should_terminate(true);
        loop {
            hlt();
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
        let stack = None;
        Self {
            tid,
            name,
            process,
            should_terminate,
            last_stack_ptr,
            state,
            stack,
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

    pub fn state(&self) -> State {
        self.state
    }

    pub fn stack(&self) -> &Option<Stack> {
        &self.stack
    }

    pub fn last_stack_ptr(&mut self) -> &mut usize {
        self.last_stack_ptr.as_mut().get_mut()
    }
}
