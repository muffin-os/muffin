use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::c_void;
use core::pin::Pin;
use core::time::Duration;

use kernel_abi::{EAGAIN, EFAULT, Errno, ThreadId, Timespec};
use kernel_syscall::UserspacePtr;
use kernel_syscall::access::{ThreadStart, UnstartedThread};
use kernel_syscall::thread::{
    sys_addr_wait, sys_addr_wake, sys_thread_create, sys_thread_exit, sys_thread_join,
    sys_thread_self,
};
use x86_64::VirtAddr;
use x86_64::registers::rflags::RFlags;
use x86_64::structures::idt::InterruptStackFrameValue;

use super::user_mem::read_user;
use crate::UsizeExt;
use crate::arch::gdt::Selectors;
use crate::arch::idt::{SyscallRegisters, UserFrame, resume_user_frame};
use crate::mcore::mtask::process::{Process, allocate_initial_fx_area};
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::{HigherHalfStack, Task};
use crate::syscall::access::KernelAccess;

/// This type is a task of a new thread that no CPU runs yet.
pub struct UnstartedTask(Pin<Box<Task>>);

impl UnstartedThread for UnstartedTask {
    fn id(&self) -> ThreadId {
        self.0.id().into()
    }
}

impl UnstartedTask {
    pub fn start(self) {
        GlobalTaskQueue::enqueue(self.0);
    }
}

/// This function builds the task of a new thread of `process`. The task gets
/// a fresh TLS block, a fresh FX area, a kernel stack and the signal mask of
/// `start`. The task has no user stack, because the caller passes its own.
///
/// # Errors
/// This function returns `EAGAIN` when an allocation fails.
pub fn create_thread(process: &Arc<Process>, start: ThreadStart) -> Result<UnstartedTask, Errno> {
    let template = process.tls_template().read().clone();
    let tls = template
        .map(|template| template.instantiate(process))
        .transpose()
        .map_err(|_| EAGAIN)?;
    let fx_area = allocate_initial_fx_area(process).ok_or(EAGAIN)?;

    let arg = Box::into_raw(Box::new(start));
    let Ok(kstack) = HigherHalfStack::allocate(16, thread_entry, arg.cast(), Task::exit) else {
        // Safety: no task holds the box, so this function owns it and can free it.
        drop(unsafe { Box::from_raw(arg) });
        return Err(EAGAIN);
    };

    let task = Task::create_with_stack(process, kstack);
    *task.tls().write() = tls;
    *task.fx_area().write() = Some(fx_area);
    task.set_signal_mask(start.signal_mask);
    Ok(UnstartedTask(Box::pin(task)))
}

/// This function is the first code of a thread that `SYS_THREAD_CREATE`
/// starts. It runs with the process address space active and interrupts
/// on, like `trampoline`. The switch to this task loads `FsBase` from its
/// TLS block.
extern "C" fn thread_entry(start: *mut c_void) {
    // Safety: `create_thread` creates the box for this task alone.
    let start = *unsafe { Box::from_raw(start.cast::<ThreadStart>()) };
    let task = Task::current();
    if task.process().reap_requested_for(task.id()) {
        Task::exit_current();
    }

    let sel = Selectors::current();
    let mut user = UserFrame {
        regs: SyscallRegisters::default(),
        frame: InterruptStackFrameValue::new(
            VirtAddr::new(start.entry.addr().into_u64()),
            sel.user_code,
            RFlags::INTERRUPT_FLAG,
            VirtAddr::new(start.stack_top.addr().into_u64()),
            sel.user_data,
        ),
    };
    user.regs.rdi = start.arg;
    // Safety: `user` lives on this kernel stack until iretq.
    unsafe { resume_user_frame(&raw const user) };
}

pub fn dispatch_sys_thread_create(
    entry: usize,
    arg: usize,
    stack_top: usize,
) -> Result<usize, Errno> {
    sys_thread_create(&KernelAccess::new(), entry, arg, stack_top)
}

pub fn dispatch_sys_thread_exit(
    flags: usize,
    unmap_addr: usize,
    unmap_len: usize,
) -> Result<usize, Errno> {
    // The access holds a reference to the process. `Task::exit_current` never
    // returns. Therefore the temporary access must drop at the end of this
    // statement.
    sys_thread_exit(&KernelAccess::new(), flags, unmap_addr, unmap_len)?;
    Task::exit_current()
}

pub fn dispatch_sys_thread_join(tid: usize) -> Result<usize, Errno> {
    sys_thread_join(&KernelAccess::new(), ThreadId::new(tid as u64))
}

pub fn dispatch_sys_thread_self() -> Result<usize, Errno> {
    Ok(sys_thread_self(&KernelAccess::new()))
}

pub fn dispatch_sys_addr_wait(
    addr: usize,
    expected: usize,
    timeout: usize,
) -> Result<usize, Errno> {
    let word = UserspacePtr::try_from_usize(addr).map_err(|_| EFAULT)?;
    let timeout = if timeout == 0 {
        None
    } else {
        Some(Duration::try_from(read_user::<Timespec>(timeout)?)?)
    };
    // The register holds the `u32` with zero extension.
    sys_addr_wait(&KernelAccess::new(), word, expected as u32, timeout)
}

pub fn dispatch_sys_addr_wake(addr: usize, count: usize) -> Result<usize, Errno> {
    let word = UserspacePtr::<u32>::try_from_usize(addr).map_err(|_| EFAULT)?;
    word.validate_range(size_of::<u32>()).map_err(|_| EFAULT)?;
    sys_addr_wake(&KernelAccess::new(), word, count)
}
