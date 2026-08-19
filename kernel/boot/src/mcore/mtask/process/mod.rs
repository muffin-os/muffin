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
use kernel_abi::ProcessId;
use kernel_memapi::{Guarded, Location, MemoryApi, UserAccessible};
use kernel_syscall::exec::build_initial_stack;
use kernel_syscall::signal::SignalState;
use kernel_vfs::OpenError;
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, ROOT};
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

use crate::file::{OpenFileDescription, vfs};
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::fd::{FdNum, FileDescriptor, FileDescriptorFlags};
use crate::mcore::mtask::process::mem::MemoryRegions;
use crate::mcore::mtask::process::telemetry::Telemetry;
use crate::mcore::mtask::process::tree::process_tree;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::{FxArea, HigherHalfStack, StackAllocationError, Task, TaskId};
use crate::mcore::mtask::wait::{
    TaskUnparkTicket, TaskWaker, block_current, reserve, sleep_until, unpark_and_enqueue, wake,
};
use crate::mem::address_space::AddressSpace;
use crate::mem::memapi::{LowerHalfAllocation, LowerHalfMemoryApi, Writable};
use crate::mem::virt::VirtualMemoryAllocator;
use crate::{U64Ext, UsizeExt};

pub(crate) mod elf;

pub mod fd;
pub mod mem;
pub mod telemetry;

pub(crate) mod tree;

static ROOT_PROCESS: OnceCell<Arc<Process>> = OnceCell::uninit();

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

/// How a process ended. Recorded once on the first terminating event.
#[derive(Debug, Copy, Clone)]
pub enum ExitOutcome {
    Exited(usize),
    Signaled(kernel_abi::Signal),
}

#[must_use]
pub enum ParkOutcome {
    Ready,
    /// An exec reap targets the caller, which must stop waiting and return.
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
    name: String,

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
    interruptible_wakers: Mutex<Vec<(TaskId, TaskWaker)>>,

    task_accounting: Mutex<TaskAccounting>,
    /// Mirror of `task_accounting.reap.is_some()`, so the timer tick pays one
    /// Acquire load instead of a lock when no exec is in progress.
    // FIXME: find a better solution than mirroring data
    reap_active: AtomicBool,

    file_descriptors: RwLock<BTreeMap<FdNum, FileDescriptor>>,

    exit_outcome: OnceCell<ExitOutcome>,
}

impl Process {
    pub fn root() -> &'static Arc<Process> {
        ROOT_PROCESS.get_or_init(|| {
            let pid = new_process_id();
            let root = Arc::new(Self {
                pid,
                name: "root".to_string(),
                ppid: RwLock::new(pid),
                executable_path: RwLock::new(None),
                executable_segments: RwLock::new(vec![]),
                current_working_directory: RwLock::new(ROOT.to_owned()),
                address_space: None,
                lower_half_memory: Arc::new(RwLock::new(VirtualMemoryManager::new(
                    VirtAddr::new(0x00),
                    0x0000_7FFF_FFFF_FFFF,
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
                file_descriptors: RwLock::new(BTreeMap::new()),
                exit_outcome: OnceCell::uninit(),
            });
            process_tree().write().processes.insert(pid, root.clone());
            root
        })
    }

    fn create_new(
        parent: &Arc<Process>,
        name: String,
        executable_path: Option<impl AsRef<AbsolutePath>>,
    ) -> Arc<Self> {
        let pid = new_process_id();
        let parent_pid = parent.pid;
        let address_space = AddressSpace::new();

        let process = Self {
            pid,
            name,
            ppid: RwLock::new(parent_pid),
            executable_path: RwLock::new(executable_path.map(|x| x.as_ref().to_owned())),
            executable_segments: RwLock::new(vec![]),
            current_working_directory: RwLock::new(parent.current_working_directory.read().clone()),
            address_space: Some(address_space),
            // Dynamic (Location::Anywhere) reservations start at 4 GiB so they
            // can never grow into the fixed ET_EXEC link region around 0x20_0000,
            // where the ELF loader must reserve exact addresses. The range ends
            // at the top of the canonical lower half.
            lower_half_memory: Arc::new(RwLock::new(VirtualMemoryManager::new(
                VirtAddr::new(0x1_0000_0000),
                0x7F00_0000_0000,
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
            file_descriptors: RwLock::new(BTreeMap::new()),
            exit_outcome: OnceCell::uninit(),
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
            .open(path)
            .map_err(CreateProcessError::OpenExecutable)?;
        elf::validate(&node)?;

        let process = Self::create_new(parent, path.to_string(), Some(path));
        Span::current().record("pid", process.pid.as_u64());

        let kstack = HigherHalfStack::allocate(16, trampoline, ptr::null_mut(), Task::exit)?;
        let main_task = Task::create_with_stack(&process, kstack);

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

    pub fn name(&self) -> &str {
        &self.name
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

    pub(crate) fn set_executable_path(&self, path: AbsoluteOwnedPath) {
        *self.executable_path.write() = Some(path);
    }

    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
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

    fn register_interruptible_waker(&self, tid: TaskId, waker: TaskWaker) {
        self.interruptible_wakers.lock().push((tid, waker));
    }

    fn clear_interruptible_waker(&self, tid: TaskId) {
        self.interruptible_wakers.lock().retain(|(t, _)| *t != tid);
    }

    pub fn wake_interruptible(&self) {
        let wakers = core::mem::take(&mut *self.interruptible_wakers.lock());
        for (_, waker) in wakers {
            wake(&waker);
        }
    }

    /// Parks the current task until `should_wake` holds or an exec reap
    /// targets it, reparking on spurious wakeups. A deadline only arms a
    /// wakeup, the closure must observe it to end the wait. Parking anywhere
    /// else hides the task from signal generation and the exec reaper, so
    /// every blocking syscall must wait through here.
    pub fn park_current_task(
        &self,
        deadline_ns: Option<u64>,
        mut should_wake: impl FnMut() -> bool,
    ) -> ParkOutcome {
        let ctx = ExecutionContext::load();
        let task = ctx.current_task();
        debug_assert_eq!(
            task.process().pid(),
            self.pid,
            "parking on a foreign process"
        );
        let tid = task.id();
        loop {
            if self.reap_requested_for(tid) {
                return ParkOutcome::Interrupted;
            }
            if should_wake() {
                return ParkOutcome::Ready;
            }
            let (park_ticket, unpark_ticket) = reserve().split();
            let waker = unpark_ticket.into_waker();
            if let Some(deadline_ns) = deadline_ns {
                sleep_until(deadline_ns, waker.clone());
            }
            self.register_interruptible_waker(tid, waker.clone());
            if should_wake() || self.reap_requested_for(tid) {
                wake(&waker);
            }
            block_current(park_ticket);
            self.clear_interruptible_waker(tid);
        }
    }

    pub(in crate::mcore::mtask) fn register_task(&self) {
        self.task_accounting.lock().live += 1;
    }

    pub(in crate::mcore::mtask) fn retire_task(&self) {
        let mut acc = self.task_accounting.lock();
        acc.live -= 1;
        if acc.live == 1
            && let Some(reap) = &mut acc.reap
            && let Some(waker) = reap.waiter.take()
        {
            wake(&waker);
        }
    }

    /// True when an exec reap targets `tid`. An observer must exit at its
    /// next safe point.
    pub(crate) fn reap_requested_for(&self, tid: TaskId) -> bool {
        if !self.reap_active.load(Ordering::Acquire) {
            return false;
        }
        self.task_accounting
            .lock()
            .reap
            .as_ref()
            .is_some_and(|r| r.keeper != tid)
    }

    /// Requests termination of every other task of the process and parks
    /// until the cleanup task has retired them, per POSIX exec semantics.
    /// A caller that is itself a reap target never returns, it exits here.
    pub fn reap_sibling_tasks(&self, keeper: TaskId) -> SoleLiveTask<'_> {
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

    /// Records the first terminating event. A process that exits while being
    /// signaled keeps the first event, so a later outcome is dropped.
    pub fn set_exit_outcome(&self, outcome: ExitOutcome) {
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
            .field("ppid", &*self.ppid.read())
            .field("name", &self.name)
            .field("address_space", self.address_space())
            .finish_non_exhaustive()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let my_ppid = *self.ppid.read();
        let mut guard = process_tree().write();
        guard
            .processes
            .remove(&self.pid)
            .expect("process should be in process tree");
        if let Some(children) = guard.children.remove(&self.pid) {
            for child in children {
                *child.ppid.write() = my_ppid;
            }
        }

        // TODO: deallocate all physical frames that are not part of a shared mapping
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
    let ctx = ExecutionContext::load();
    let current_task = ctx.scheduler().current_task();
    let current_process = current_task.process().clone();

    let executable_path = current_process
        .executable_path()
        .expect("should have an executable path");
    let node = vfs()
        .write()
        .open(&executable_path)
        .expect("should be able to open executable");
    let validated = elf::validate(&node).expect("should be able to validate executable");

    {
        let mut guard = current_process.file_descriptors.write();

        let devnull = vfs()
            .read()
            .open(AbsolutePath::try_new("/dev/null").unwrap())
            .expect("should be able to open /dev/null");
        let devnull_ofd = Arc::new(OpenFileDescription::from(devnull));
        guard.insert(
            0.into(),
            FileDescriptor::new(0.into(), FileDescriptorFlags::empty(), devnull_ofd.clone()),
        );

        let devserial = vfs()
            .read()
            .open(AbsolutePath::try_new("/dev/serial").unwrap())
            .expect("should be able to open /dev/serial");
        let devserial_ofd = Arc::new(OpenFileDescription::from(devserial));
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

    let sel = ctx.selectors();

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
            UserAccessible::Yes,
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

    let fx_area = memapi
        .allocate(
            Location::Anywhere,
            Layout::new::<FxArea>(),
            UserAccessible::Yes,
            Guarded::No,
        )
        .ok_or(elf::LoadExecutableError::AllocationFailed)?;
    let fx_area_ptr = fx_area.start().as_mut_ptr::<u8>();
    unsafe {
        fx_area_ptr.copy_from_nonoverlapping(INITIAL_FX_IMAGE.as_ptr(), INITIAL_FX_IMAGE.len());
    }
    *task.fx_area().write() = Some(fx_area);
    // TS parks the seeded image until the first user FPU instruction. Its #NM
    // handler restores it, and the scheduler skips saving the live FPU state
    // into the fresh area while TS is set.
    unsafe {
        Cr0::update(|cr0| cr0.insert(Cr0Flags::TASK_SWITCHED));
    }

    Ok((VirtAddr::new(entry as u64), VirtAddr::new(rsp as u64)))
}
