use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::ffi::c_void;
use core::fmt::{Debug, Formatter};
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use conquer_once::spin::OnceCell;
use kernel_abi::{FdNum, ProcessId, Signal};
use kernel_memapi::{Guarded, Location, MemoryApi};
use kernel_syscall::access::AccessMode;
use kernel_syscall::exec::build_initial_stack;
use kernel_syscall::signal::{SignalMask, SignalState, thread_to_wake};
use kernel_syscall::thread::{AddrWaitQueues, ThreadTable};
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, ROOT};
use kernel_vfs::{CreateMode, OpenError};
use kernel_virtual_memory::VirtualMemoryManager;
use spin::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use thiserror::Error;
use tracing::field::Empty;
use tracing::{Level, Span, debug, instrument};
use x86_64::VirtAddr;
use x86_64::registers::control::{Cr0, Cr0Flags};
use x86_64::registers::rflags::RFlags;
use x86_64::structures::idt::InterruptStackFrameValue;
use x86_64::structures::paging::{PageSize, Size4KiB};

use crate::arch::gdt::Selectors;
use crate::file::{OpenFile, OpenFileDescription, vfs};
use crate::hpet::Instant;
use crate::mcore::mtask::process::fd::{FileDescriptor, FileDescriptorFlags};
use crate::mcore::mtask::process::mem::MemoryRegions;
use crate::mcore::mtask::process::telemetry::Telemetry;
use crate::mcore::mtask::process::tls::TlsTemplate;
use crate::mcore::mtask::process::tree::process_tree;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::{FxArea, HigherHalfStack, StackAllocationError, Task, TaskId};
use crate::mcore::mtask::wait::{
    TaskUnparkTicket, TaskWaker, WaitQueue, block_current, reserve, sleep_until,
    unpark_and_enqueue, wake,
};
use crate::mem::address_space::AddressSpace;
use crate::mem::memapi::{
    LowerHalfAllocOptions, LowerHalfAllocation, LowerHalfMemoryApi, Writable,
};
use crate::mem::virt::VirtualMemoryAllocator;
use crate::{U64Ext, UsizeExt};

pub(crate) mod elf;

pub mod fd;
pub mod mem;
pub mod telemetry;
pub mod tls;

pub(crate) mod tree;

static ROOT_PROCESS: OnceCell<Arc<Process>> = OnceCell::uninit();

// Exclusive end of the canonical lower half, where the user address
// space ends.
const LOWER_HALF_END: u64 = 0x8000_0000_0000;

// Dynamic (Location::Anywhere) reservations start at 4 GiB so they can
// never grow into the fixed ET_EXEC link region around 0x20_0000, where
// the ELF loader must reserve exact addresses.
const DYNAMIC_LOWER_HALF_START: u64 = 0x1_0000_0000;

pub fn new_process_id() -> ProcessId {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    ProcessId::from(COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Everything the process signals lock protects.
///
/// `stop_unpark` lives behind the same lock as the signal state, so stop
/// parking (tick delivery) and resume (signal generation or an exec reap)
/// are serialized by construction.
#[derive(Default)]
pub struct Signals {
    state: SignalState,
    stop_unpark: Vec<TaskUnparkTicket>,
}

impl Signals {
    pub fn store_stop_unpark(&mut self, ticket: TaskUnparkTicket) {
        self.stop_unpark.push(ticket);
    }

    pub fn resume_stopped_tasks(&mut self) {
        for ticket in self.stop_unpark.drain(..) {
            unpark_and_enqueue(ticket);
        }
    }
}

impl Deref for Signals {
    type Target = SignalState;

    fn deref(&self) -> &SignalState {
        &self.state
    }
}

impl DerefMut for Signals {
    fn deref_mut(&mut self) -> &mut SignalState {
        &mut self.state
    }
}

pub type SignalsWriteGuard<'a> = RwLockWriteGuard<'a, Signals>;

pub use kernel_syscall::access::ExitOutcome;

#[must_use]
pub enum ParkOutcome {
    Ready,
    /// A reap or the end of the process targets the caller. The caller must
    /// stop waiting and return.
    Interrupted,
}

struct TaskAccounting {
    live: usize,
    reap: Option<ReapState>,
}

struct ReapState {
    keeper: TaskId,
    waiter: Option<TaskWaker>,
}

/// This type describes a task in an interruptible wait. A task changes its
/// mask only while it runs. Therefore `mask` stays exact while the task waits.
struct InterruptibleWaiter {
    tid: TaskId,
    mask: SignalMask,
    waker: TaskWaker,
}

/// Proof that the calling task is the process's only live task. Constructed
/// only by [`Process::reap_sibling_tasks`].
pub struct SoleLiveTask<'p> {
    process: &'p Process,
}

impl SoleLiveTask<'_> {
    pub fn finish_reap(self) {
        self.process.reap_active.store(false, Ordering::Release);
        self.process.task_accounting.lock().reap = None;
    }
}

pub struct Process {
    pid: ProcessId,
    name: RwLock<String>,

    ppid: RwLock<ProcessId>,

    executable_path: RwLock<Option<AbsoluteOwnedPath>>,
    executable_segments: RwLock<Vec<LowerHalfAllocation<Writable>>>,
    current_working_directory: RwLock<AbsoluteOwnedPath>,

    address_space: Option<AddressSpace>,
    lower_half_memory: Arc<RwLock<VirtualMemoryManager>>,

    telemetry: Telemetry,

    memory_regions: MemoryRegions,

    signals: RwLock<Signals>,

    /// Never held across any other lock acquisition. The kill path takes it
    /// while holding the signals write guard, the sleeping task takes it
    /// bare, so no inversion exists as long as every accessor is a
    /// self-contained lock-store-unlock.
    interruptible_wakers: Mutex<Vec<InterruptibleWaiter>>,

    task_accounting: Mutex<TaskAccounting>,
    /// Mirror of `task_accounting.reap.is_some()`, so the timer tick pays one
    /// Acquire load instead of a lock when no exec is in progress.
    reap_active: AtomicBool,
    /// A fatal signal sets this flag when it ends the process. Every task
    /// exits at its next safe point. The flag never clears.
    terminating: AtomicBool,

    file_descriptors: RwLock<BTreeMap<FdNum, FileDescriptor>>,

    tls_template: RwLock<Option<TlsTemplate>>,
    threads: Mutex<ThreadTable>,
    thread_exits: WaitQueue,
    addr_waiters: Mutex<AddrWaitQueues<WaitQueue>>,

    exit_outcome: OnceCell<ExitOutcome>,

    /// User memory guards hold this lock. Only `syscall::user_mem` acquires it.
    pub(crate) user_memory: RwLock<()>,
}

impl Process {
    /// The process of the task currently running on this CPU.
    #[must_use]
    pub fn current() -> &'static Arc<Process> {
        Task::current().process()
    }

    pub fn root() -> &'static Arc<Process> {
        ROOT_PROCESS.get_or_init(|| {
            let pid = new_process_id();
            let root = Arc::new(Self {
                pid,
                name: RwLock::new("root".to_string()),
                ppid: RwLock::new(pid),
                executable_path: RwLock::new(None),
                executable_segments: RwLock::new(vec![]),
                current_working_directory: RwLock::new(ROOT.to_owned()),
                address_space: None,
                lower_half_memory: Arc::new(RwLock::new(VirtualMemoryManager::new(
                    VirtAddr::zero(),
                    LOWER_HALF_END,
                ))),
                telemetry: Telemetry::default(),
                memory_regions: MemoryRegions::new(),
                signals: RwLock::new(Signals::default()),
                interruptible_wakers: Mutex::new(vec![]),
                task_accounting: Mutex::new(TaskAccounting {
                    live: 0,
                    reap: None,
                }),
                reap_active: AtomicBool::new(false),
                terminating: AtomicBool::new(false),
                file_descriptors: RwLock::new(BTreeMap::new()),
                tls_template: RwLock::new(None),
                threads: Mutex::new(ThreadTable::default()),
                thread_exits: WaitQueue::new(),
                addr_waiters: Mutex::new(AddrWaitQueues::new()),
                exit_outcome: OnceCell::uninit(),
                user_memory: RwLock::new(()),
            });
            process_tree().write().processes.insert(pid, root.clone());
            root
        })
    }

    /// Creates a process with a fresh address space, an empty lower-half
    /// allocator and no memory regions. It inserts the process into the
    /// process tree. The process has no task yet.
    pub fn create_new(
        parent: &Arc<Process>,
        name: String,
        executable_path: Option<impl AsRef<AbsolutePath>>,
    ) -> Arc<Self> {
        let pid = new_process_id();
        let parent_pid = parent.pid;
        let address_space = AddressSpace::new(parent.address_space());

        let process = Self {
            pid,
            name: RwLock::new(name),
            ppid: RwLock::new(parent_pid),
            executable_path: RwLock::new(executable_path.map(|x| x.as_ref().to_owned())),
            executable_segments: RwLock::new(vec![]),
            current_working_directory: RwLock::new(parent.current_working_directory.read().clone()),
            address_space: Some(address_space),
            lower_half_memory: Arc::new(RwLock::new(VirtualMemoryManager::new(
                VirtAddr::new(DYNAMIC_LOWER_HALF_START),
                LOWER_HALF_END - DYNAMIC_LOWER_HALF_START,
            ))),
            telemetry: Telemetry::default(),
            memory_regions: MemoryRegions::new(),
            signals: RwLock::new(Signals::default()),
            interruptible_wakers: Mutex::new(vec![]),
            task_accounting: Mutex::new(TaskAccounting {
                live: 0,
                reap: None,
            }),
            reap_active: AtomicBool::new(false),
            terminating: AtomicBool::new(false),
            file_descriptors: RwLock::new(BTreeMap::new()),
            tls_template: RwLock::new(None),
            threads: Mutex::new(ThreadTable::default()),
            thread_exits: WaitQueue::new(),
            addr_waiters: Mutex::new(AddrWaitQueues::new()),
            exit_outcome: OnceCell::uninit(),
            user_memory: RwLock::new(()),
        };

        let res = Arc::new(process);
        process_tree().write().processes.insert(pid, res.clone());
        res
    }

    #[instrument(
        level = Level::INFO,
        skip_all,
        fields(path = %path.as_ref(), ppid = %parent.pid(), pid = Empty)
    )]
    pub fn create_unscheduled(
        parent: &Arc<Process>,
        path: impl AsRef<AbsolutePath>,
    ) -> Result<(Arc<Self>, Task), CreateProcessError> {
        let path = path.as_ref();
        let node = vfs()
            .read()
            .open(path, CreateMode::Never)
            .map_err(CreateProcessError::OpenExecutable)?;
        elf::validate(&node)?;

        let process = Self::create_new(parent, path.to_string(), Some(path));
        Span::current().record("pid", process.pid.as_u64());

        let kstack = HigherHalfStack::allocate(16, trampoline, ptr::null_mut(), Task::exit)?;
        let main_task = Task::create_with_stack(&process, kstack);
        process.threads.lock().insert(main_task.id().into());

        Ok((process, main_task))
    }

    /// Enqueues the new process's main task. The executable is opened and statically validated
    /// before any process is inserted into the process tree.
    pub fn create_from_executable(
        parent: &Arc<Process>,
        path: impl AsRef<AbsolutePath>,
    ) -> Result<Arc<Self>, CreateProcessError> {
        let (process, main_task) = Self::create_unscheduled(parent, path)?;
        GlobalTaskQueue::enqueue(Box::pin(main_task));

        Ok(process)
    }

    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    pub fn ppid(&self) -> ProcessId {
        *self.ppid.read()
    }

    pub fn process_group_id(&self) -> ProcessId {
        // TODO(tpdenk): return the group that setpgid sets, once setpgid exists
        self.pid
    }

    pub fn name(&self) -> String {
        self.name.read().clone()
    }

    pub fn file_descriptors(&self) -> &RwLock<BTreeMap<FdNum, FileDescriptor>> {
        &self.file_descriptors
    }

    pub fn address_space(&self) -> &AddressSpace {
        self.address_space
            .as_ref()
            .unwrap_or(AddressSpace::kernel())
    }

    pub fn vmm(self: &Arc<Self>) -> impl VirtualMemoryAllocator {
        self.lower_half_memory.clone()
    }

    pub fn current_working_directory(&self) -> &RwLock<AbsoluteOwnedPath> {
        &self.current_working_directory
    }

    pub fn memory_regions(&self) -> &MemoryRegions {
        &self.memory_regions
    }

    pub fn executable_segments(&self) -> &RwLock<Vec<LowerHalfAllocation<Writable>>> {
        &self.executable_segments
    }

    /// The root process has no executable, so this returns `None` for it.
    pub fn executable_path(&self) -> Option<AbsoluteOwnedPath> {
        self.executable_path.read().clone()
    }

    /// Fork copies the parent name, so exec must replace it.
    pub(crate) fn set_executable(&self, path: AbsoluteOwnedPath) {
        *self.name.write() = path.to_string();
        *self.executable_path.write() = Some(path);
    }

    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// This function returns the TLS image of the executable. Each new thread gets a block from it.
    pub fn tls_template(&self) -> &RwLock<Option<TlsTemplate>> {
        &self.tls_template
    }

    pub fn threads(&self) -> &Mutex<ThreadTable> {
        &self.threads
    }

    /// This function returns the queue that each thread exit wakes. The joiners then check again.
    pub fn thread_exits(&self) -> &WaitQueue {
        &self.thread_exits
    }

    pub fn addr_waiters(&self) -> &Mutex<AddrWaitQueues<WaitQueue>> {
        &self.addr_waiters
    }

    pub fn signals_read(&self) -> RwLockReadGuard<'_, Signals> {
        self.signals.read()
    }

    pub fn try_signals_read(&self) -> Option<RwLockReadGuard<'_, Signals>> {
        self.signals.try_read()
    }

    pub fn signals_write(&self) -> SignalsWriteGuard<'_> {
        self.signals.write()
    }

    pub fn try_signals_write(&self) -> Option<SignalsWriteGuard<'_>> {
        self.signals.try_write()
    }

    fn register_interruptible_waker(&self, tid: TaskId, mask: SignalMask, waker: TaskWaker) {
        self.interruptible_wakers
            .lock()
            .push(InterruptibleWaiter { tid, mask, waker });
    }

    fn clear_interruptible_waker(&self, tid: TaskId) {
        self.interruptible_wakers.lock().retain(|w| w.tid != tid);
    }

    pub fn wake_interruptible(&self) {
        let waiters = core::mem::take(&mut *self.interruptible_wakers.lock());
        for waiter in waiters {
            wake(&waiter.waker);
        }
    }

    /// This function wakes the one waiting task that must take the new
    /// process-pending `signo`. [`thread_to_wake`] chooses that task. `sender`
    /// is the mask of the sending task when the sending task belongs to this
    /// process.
    pub fn wake_signal_taker(&self, signo: Signal, sender: Option<SignalMask>) {
        let waiter = {
            let mut waiters = self.interruptible_wakers.lock();
            thread_to_wake(
                signo,
                sender,
                waiters.iter().enumerate().map(|(i, w)| (i, w.mask)),
            )
            .map(|i| waiters.swap_remove(i))
        };
        if let Some(waiter) = waiter {
            wake(&waiter.waker);
        }
    }

    /// This function parks the current task until `should_wake` returns true,
    /// or until process termination or a reap targets the task.
    /// The predicate follows the contract of [`kernel_syscall::access::ThreadAccess::park`].
    /// A `deadline` or a `queue` only arms a wakeup. `should_wake` decides.
    pub fn park_current_task(
        &self,
        deadline: Option<Instant>,
        queue: Option<&WaitQueue>,
        mut should_wake: impl FnMut() -> bool,
    ) -> ParkOutcome {
        let task = Task::current();
        debug_assert_eq!(
            task.process().pid(),
            self.pid,
            "parking on a foreign process"
        );
        debug_assert_eq!(
            task.user_memory_pins().load(Ordering::Relaxed),
            0,
            "parked while user memory is pinned"
        );
        let tid = task.id();
        let mask = task.signal_mask();
        loop {
            if self.reap_requested_for(tid) {
                return ParkOutcome::Interrupted;
            }
            if should_wake() {
                return ParkOutcome::Ready;
            }
            let (park_ticket, unpark_ticket) = reserve().split();
            let waker = unpark_ticket.into_waker();
            if let Some(deadline) = deadline {
                sleep_until(deadline, waker.clone());
            }
            self.register_interruptible_waker(tid, mask, waker.clone());
            if let Some(queue) = queue {
                queue.register(waker.clone());
            }
            // The condition can become true before the wakers are armed. The
            // block on the woken ticket consumes the ticket, so the
            // registrations above stay balanced.
            let ready = should_wake();
            if ready || self.reap_requested_for(tid) {
                wake(&waker);
            }
            block_current(park_ticket);
            self.clear_interruptible_waker(tid);
            if ready {
                return ParkOutcome::Ready;
            }
        }
    }

    pub(in crate::mcore::mtask) fn register_task(&self) {
        self.task_accounting.lock().live += 1;
    }

    pub(in crate::mcore::mtask) fn retire_task(&self) {
        let live = {
            let mut acc = self.task_accounting.lock();
            acc.live -= 1;
            if acc.live == 1
                && let Some(reap) = &mut acc.reap
                && let Some(waker) = reap.waiter.take()
            {
                wake(&waker);
            }
            acc.live
        };
        if live == 0 {
            self.threads.lock().clear();
            self.addr_waiters.lock().clear();
        } else if self.terminating.load(Ordering::Acquire) {
            // `terminate_all_tasks` runs in interrupt context, where it is
            // unsafe to take these locks. Instead, the cleanup of each task
            // wakes the waiting siblings when the task ends.
            self.signals_write().resume_stopped_tasks();
            self.wake_interruptible();
        }
    }

    /// This function returns true when a reap or the end of the process
    /// targets `tid`. The observing task must exit at its next safe point.
    pub(crate) fn reap_requested_for(&self, tid: TaskId) -> bool {
        if self.terminating.load(Ordering::Acquire) {
            return true;
        }
        if !self.reap_active.load(Ordering::Acquire) {
            return false;
        }
        self.task_accounting
            .lock()
            .reap
            .as_ref()
            .is_some_and(|r| r.keeper != tid)
    }

    /// This function makes every task of the process exit at its next safe
    /// point, for a fatal signal. Interrupt context may call this function,
    /// because it only sets a flag.
    pub fn terminate_all_tasks(&self) {
        self.terminating.store(true, Ordering::Release);
    }

    /// This function requests termination of every other task of the process.
    /// It then parks until the cleanup task retires them, as POSIX exec
    /// and exit semantics require. A caller that is itself a reap target
    /// never returns. It exits here.
    pub fn reap_sibling_tasks(&self, keeper: TaskId) -> SoleLiveTask<'_> {
        if self.terminating.load(Ordering::Acquire) {
            Task::exit_current();
        }
        {
            let mut acc = self.task_accounting.lock();
            match &acc.reap {
                Some(reap) if reap.keeper != keeper => {
                    drop(acc);
                    Task::exit_current();
                }
                Some(_) => {}
                None => {
                    acc.reap = Some(ReapState {
                        keeper,
                        waiter: None,
                    });
                    self.reap_active.store(true, Ordering::Release);
                }
            }
        }

        self.signals_write().resume_stopped_tasks();
        self.wake_interruptible();

        loop {
            if self.task_accounting.lock().live == 1 {
                return SoleLiveTask { process: self };
            }
            let (park_ticket, unpark_ticket) = reserve().split();
            let waker = unpark_ticket.into_waker();
            {
                let mut acc = self.task_accounting.lock();
                if acc.live == 1 {
                    wake(&waker);
                } else if let Some(reap) = &mut acc.reap {
                    reap.waiter = Some(waker.clone());
                }
            }
            block_current(park_ticket);
        }
    }

    /// This function drops the exit records and the address waiters that the
    /// reaped siblings leave. Then `keeper` is the only thread of the process.
    pub fn reset_threads(&self, keeper: TaskId, _sole: &SoleLiveTask<'_>) {
        self.threads.lock().reset_to(keeper.into());
        self.addr_waiters.lock().clear();
    }

    /// Records the first terminating event. A process that exits while being
    /// signaled keeps the first event, so a later outcome is dropped.
    ///
    /// The children move to the reaper before the outcome becomes visible. A
    /// waiter that sees the outcome then never finds a child with a dead parent.
    pub fn set_exit_outcome(&self, outcome: ExitOutcome) {
        // Shutdown SIGTERMs init like every other process.
        if self.pid == ProcessId::REAPER && !crate::power::is_shutdown_in_progress() {
            panic!("init (pid {}) exited: {outcome:?}", self.pid);
        }
        for child in process_tree().read().all().filter(|p| p.ppid() == self.pid) {
            *child.ppid.write() = ProcessId::REAPER;
        }
        let _ = self.exit_outcome.try_init_once(|| outcome);
    }

    pub fn exit_outcome(&self) -> Option<ExitOutcome> {
        self.exit_outcome.get().copied()
    }
}

impl Debug for Process {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Process")
            .field("pid", &self.pid)
            .field("ppid", &self.ppid())
            .field("name", &*self.name.read())
            .field("address_space", self.address_space())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum CreateProcessError {
    #[error("failed to open the executable")]
    OpenExecutable(#[from] OpenError),
    #[error(transparent)]
    LoadExecutable(#[from] elf::LoadExecutableError),
    #[error("failed to allocate stack")]
    StackAllocationError(#[from] StackAllocationError),
}

/// The FPU state a task starts with, in the architectural 512 byte FXSAVE layout.
///
/// A fresh task must not inherit the kernel's live XMM registers or MXCSR, and the
/// backing allocation arrives uninitialised, so all 512 bytes are written. Fields
/// not named below stay zero, which empties ST0-7 (offsets 32..160) and zeroes
/// XMM0-15 (offsets 160..416). FTW at offset 4 stays zero, marking all x87
/// registers empty. `0x1F80` sets no reserved MXCSR bits, so `fxrstor` accepts it.
const INITIAL_FX_IMAGE: [u8; 512] = {
    let mut image = [0u8; 512];
    // FCW at offset 0, the x87 default with every exception masked
    image[0] = 0x7F;
    image[1] = 0x03;
    // MXCSR at offset 24, every SSE exception masked, round to nearest
    image[24] = 0x80;
    image[25] = 0x1F;
    // MXCSR_MASK at offset 28
    image[28] = 0xFF;
    image[29] = 0xFF;
    image
};

extern "C" fn trampoline(_arg: *mut c_void) {
    let current_task = Task::current();
    let current_process = current_task.process().clone();

    let executable_path = current_process
        .executable_path()
        .expect("should have an executable path");
    let node = vfs()
        .write()
        .open(&executable_path, CreateMode::Never)
        .expect("should be able to open executable");
    let validated = elf::validate(&node).expect("should be able to validate executable");

    {
        let mut guard = current_process.file_descriptors.write();

        let devnull = vfs()
            .read()
            .open(
                AbsolutePath::try_new("/dev/null").unwrap(),
                CreateMode::Never,
            )
            .expect("should be able to open /dev/null");
        let devnull_ofd = Arc::new(OpenFileDescription::File(OpenFile::new(
            devnull,
            AccessMode::ReadOnly,
            false,
        )));
        guard.insert(
            0.into(),
            FileDescriptor::new(0.into(), FileDescriptorFlags::empty(), devnull_ofd.clone()),
        );

        let devserial = vfs()
            .read()
            .open(
                AbsolutePath::try_new("/dev/serial").unwrap(),
                CreateMode::Never,
            )
            .expect("should be able to open /dev/serial");
        let devserial_ofd = Arc::new(OpenFileDescription::File(OpenFile::new(
            devserial,
            AccessMode::WriteOnly,
            false,
        )));
        guard.insert(
            1.into(),
            FileDescriptor::new(
                1.into(),
                FileDescriptorFlags::empty(),
                devserial_ofd.clone(),
            ),
        );
        guard.insert(
            2.into(),
            FileDescriptor::new(
                2.into(),
                FileDescriptorFlags::empty(),
                devserial_ofd.clone(),
            ),
        );
    }

    let (entry, rsp) = setup_user_image(
        &current_process,
        current_task,
        &validated,
        &node,
        &[executable_path.as_str().as_bytes()],
        &[],
    )
    .expect("should be able to load executable");

    let sel = Selectors::current();

    debug!("stack_ptr: {:p}", rsp.as_ptr::<u8>());
    debug!("code_ptr: {:p}", entry.as_ptr::<u8>());

    let isfv = InterruptStackFrameValue::new(
        entry,
        sel.user_code,
        RFlags::INTERRUPT_FLAG,
        rsp,
        sel.user_data,
    );
    unsafe { isfv.iretq() };
}

/// Loads `validated` into the process, replacing the task's user stack, TLS,
/// and FX state. Returns the entry rip and rsp for Ring 3.
/// `rsp` points at argc and is 16 byte aligned.
pub(crate) fn setup_user_image(
    process: &Arc<Process>,
    task: &Task,
    validated: &elf::ValidatedExecutable,
    node: &VfsNode,
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Result<(VirtAddr, VirtAddr), elf::LoadExecutableError> {
    let entry = validated.load(process, task, node)?;

    let mut memapi = LowerHalfMemoryApi::new(process.clone());
    let mut ustack_allocation = memapi
        .allocate(
            Location::Anywhere,
            Layout::from_size_align(
                Size4KiB::SIZE.into_usize() * 256,
                Size4KiB::SIZE.into_usize(),
            )
            .map_err(|_| elf::LoadExecutableError::InvalidSizeOrAlign)?,
            LowerHalfAllocOptions::USER_ACCESSIBLE
                | LowerHalfAllocOptions::FORK_COPY_EAGER
                | LowerHalfAllocOptions::PER_THREAD,
            Guarded::Yes,
        )
        .ok_or(elf::LoadExecutableError::AllocationFailed)?;

    let ustack_top = ustack_allocation.start() + ustack_allocation.len().into_u64();
    assert!(ustack_top.is_aligned(16_u64));
    let rsp = build_initial_stack(
        ustack_allocation.as_mut(),
        ustack_top.as_u64().into_usize(),
        argv,
        envp,
    )
    .ok_or(elf::LoadExecutableError::AllocationFailed)?;
    *task.ustack().write() = Some(ustack_allocation);

    let fx_area =
        allocate_initial_fx_area(process).ok_or(elf::LoadExecutableError::AllocationFailed)?;
    // TS parks the seeded image until the first user FPU instruction. Its #NM
    // handler restores it, and the scheduler skips saving the live FPU state
    // into the fresh area while TS is set, so TS must be set before the area
    // is published or a tick in between would overwrite the seeded image.
    unsafe {
        Cr0::update(|cr0| cr0.insert(Cr0Flags::TASK_SWITCHED));
    }
    *task.fx_area().write() = Some(fx_area);

    Ok((entry, VirtAddr::new(rsp as u64)))
}

/// This function allocates the FX area of a fresh thread in `process` and
/// seeds it with [`INITIAL_FX_IMAGE`]. It returns `None` when no memory is
/// available.
pub(crate) fn allocate_initial_fx_area(
    process: &Arc<Process>,
) -> Option<LowerHalfAllocation<Writable>> {
    let fx_area = LowerHalfMemoryApi::new(process.clone()).allocate(
        Location::Anywhere,
        Layout::new::<FxArea>(),
        LowerHalfAllocOptions::USER_ACCESSIBLE
            | LowerHalfAllocOptions::FORK_COPY_EAGER
            | LowerHalfAllocOptions::PER_THREAD,
        Guarded::No,
    )?;
    // Safety: the area is a fresh, resident and writable allocation of 512
    // bytes. This size is the size of the image.
    unsafe {
        fx_area
            .start()
            .as_mut_ptr::<u8>()
            .copy_from_nonoverlapping(INITIAL_FX_IMAGE.as_ptr(), INITIAL_FX_IMAGE.len());
    }
    Some(fx_area)
}
