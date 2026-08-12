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
use core::sync::atomic::{AtomicU64, Ordering};

use conquer_once::spin::OnceCell;
use kernel_abi::ProcessId;
use kernel_memapi::{Guarded, Location, MemoryApi, UserAccessible};
use kernel_syscall::exec::build_initial_stack;
use kernel_syscall::signal::SignalState;
use kernel_vfs::node::VfsNode;
use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, ROOT};
use kernel_virtual_memory::VirtualMemoryManager;
use spin::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use thiserror::Error;
use tracing::debug;
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
use crate::mcore::mtask::task::{FxArea, HigherHalfStack, StackAllocationError, Task};
use crate::mcore::mtask::wait::{TaskUnparkTicket, TaskWaker, unpark_and_enqueue, wake};
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
/// parking (tick delivery) and resume (signal generation) are serialized by
/// construction.
#[derive(Default)]
pub struct Signals {
    state: SignalState,
    stop_unpark: Option<TaskUnparkTicket>,
}

impl Signals {
    /// Keeps the unpark ticket of the stop-parked task, so a later
    /// `Continue` or `Kill` can release it.
    pub fn store_stop_unpark(&mut self, ticket: TaskUnparkTicket) {
        self.stop_unpark = Some(ticket);
    }

    pub fn resume_stopped_task(&mut self) {
        if let Some(ticket) = self.stop_unpark.take() {
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
    interruptible_waker: Mutex<Option<TaskWaker>>,

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
                interruptible_waker: Mutex::new(None),
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
            interruptible_waker: Mutex::new(None),
            file_descriptors: RwLock::new(BTreeMap::new()),
            exit_outcome: OnceCell::uninit(),
        };

        let res = Arc::new(process);
        process_tree().write().processes.insert(pid, res.clone());
        res
    }

    // TODO: add documentation
    #[allow(clippy::missing_errors_doc)]
    pub fn create_from_executable(
        parent: &Arc<Process>,
        path: impl AsRef<AbsolutePath>,
    ) -> Result<Arc<Self>, CreateProcessError> {
        // TODO: validate that the executable exists and is a valid executable file

        let path = path.as_ref();
        let process = Self::create_new(parent, path.to_string(), Some(path));
        {
            // register STDIN, STDOUT and STDERR
            let mut fds = process.file_descriptors().write();

            for (i, path) in ["/dev/stdin", "/dev/stdout", "/dev/stderr"]
                .iter()
                .map(|v| AbsolutePath::try_new(v).unwrap())
                .enumerate()
            {
                let node = vfs()
                    .write()
                    .open(path)
                    .expect("should be able to open stdin");
                let ofd = OpenFileDescription::from(node);
                let fd_num = FdNum::from(i as i32);
                let fd = FileDescriptor::new(fd_num, FileDescriptorFlags::empty(), ofd.into());
                fds.insert(fd_num, fd);
            }
        }

        let kstack = HigherHalfStack::allocate(16, trampoline, ptr::null_mut(), Task::exit)?;
        let main_task = Task::create_with_stack(&process, kstack);
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

    pub fn register_interruptible_waker(&self, waker: TaskWaker) {
        *self.interruptible_waker.lock() = Some(waker);
    }

    pub fn clear_interruptible_waker(&self) {
        self.interruptible_waker.lock().take();
    }

    pub fn wake_interruptible(&self) {
        if let Some(waker) = self.interruptible_waker.lock().take() {
            wake(&waker);
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
