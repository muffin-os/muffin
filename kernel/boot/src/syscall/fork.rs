use alloc::boxed::Box;
use alloc::sync::Arc;
use core::arch::x86_64::_fxsave64;
use core::ffi::c_void;

use kernel_abi::{EAGAIN, ENOMEM, Errno, ProcessId, Signal};
use spin::RwLock;
use x86_64::registers::control::{Cr0, Cr0Flags};
use x86_64::structures::idt::InterruptStackFrame;

use crate::arch::idt::{SyscallRegisters, UserFrame, resume_user_frame};
use crate::arch::signal::terminate_current;
use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::process::fd::FileDescriptor;
use crate::mcore::mtask::process::mem::CloneRegionError;
use crate::mcore::mtask::process::tree::process_tree;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::Task;
use crate::mem::memapi::{LowerHalfAllocation, Writable};

/// This function implements POSIX fork. The child is a copy of the calling
/// process with one task. That task is a copy of the calling thread. It
/// resumes at the `int 0x80` return of the parent with rax = 0.
///
/// # Errors
/// This function returns `ENOMEM` when it cannot clone the regions. It
/// returns `EAGAIN` when it cannot allocate the kernel stack of the child.
///
/// # Panics
/// This function panics if a task handle of the caller has no counterpart in
/// the child. A successful region clone rules this out.
pub fn dispatch_sys_fork(
    frame: &InterruptStackFrame,
    regs: &SyscallRegisters,
) -> Result<usize, Errno> {
    let parent = Process::current().clone();
    let task = Task::current();

    // The eager copy of the FX area must carry the live FPU state. With TS
    // set the last switch already spilled it into the area.
    if !Cr0::read().contains(Cr0Flags::TASK_SWITCHED)
        && let Some(fx) = task.fx_area().read().as_ref()
    {
        // Safety: the FX area is a resident, writable, 16 byte aligned 512
        // byte allocation of this task.
        unsafe { _fxsave64(fx.start().as_mut_ptr()) };
    }

    let child = Process::create_new(&parent, parent.name(), parent.executable_path());

    if clone_regions(&parent, task, &child).is_err() {
        return Err(discard(child.pid(), ENOMEM));
    }

    let mut user = Box::new(UserFrame {
        regs: *regs,
        frame: **frame,
    });
    user.regs.rax = 0;
    let user = Box::into_raw(user);
    let Ok(child_task) = Task::create_new(&child, fork_child_entry, user.cast()) else {
        // Safety: the box was never handed to a task.
        drop(unsafe { Box::from_raw(user) });
        return Err(discard(child.pid(), EAGAIN));
    };

    *child_task.ustack().write() = counterpart(task.ustack(), &child);
    *child_task.tls().write() = task.tls().read().as_ref().map(|tls| {
        tls.counterpart(&child)
            .expect("every region of the parent was cloned into the child")
    });
    *child_task.fx_area().write() = counterpart(task.fx_area(), &child);
    child_task.set_signal_mask(task.signal_mask());
    *child.tls_template().write() = parent.tls_template().read().clone();
    child.threads().lock().insert(child_task.id().into());

    {
        let mut fds = child.file_descriptors().write();
        for (num, fd) in parent.file_descriptors().read().iter() {
            fds.insert(
                *num,
                FileDescriptor::new(*num, fd.flags(), fd.file_description().clone()),
            );
        }
    }
    **child.signals_write() = parent.signals_read().fork_inherit();

    GlobalTaskQueue::enqueue(Box::pin(child_task));
    Ok(child.pid().as_u64() as usize)
}

/// This function clones the process regions and the per-thread regions of the
/// caller into `child`.
/// See [`MemoryRegion::is_per_thread`](crate::mcore::mtask::process::mem::MemoryRegion::is_per_thread).
///
/// # Errors
/// This function returns the first error of a region clone.
fn clone_regions(
    parent: &Arc<Process>,
    task: &Task,
    child: &Arc<Process>,
) -> Result<(), CloneRegionError> {
    let _exclusive = parent.exclusive_user_memory();
    let address_space = parent.address_space();
    let vmm = child.vmm();
    parent
        .memory_regions()
        .clone_into(address_space, child.memory_regions(), &vmm)?;

    for start in task.per_thread_block_starts() {
        let region = parent
            .memory_regions()
            .region_for(start)
            .expect("a live allocation has a region");
        child
            .memory_regions()
            .add_region(region.clone_into(address_space, &vmm)?);
    }
    Ok(())
}

/// Removes a child that never ran from the process tree. Its level 4 page
/// table frame leaks, like the one of every exited process.
fn discard(pid: ProcessId, errno: Errno) -> Errno {
    process_tree().write().processes.remove(&pid);
    errno
}

fn counterpart(
    slot: &RwLock<Option<LowerHalfAllocation<Writable>>>,
    child: &Arc<Process>,
) -> Option<LowerHalfAllocation<Writable>> {
    slot.read().as_ref().map(|allocation| {
        allocation
            .counterpart(child)
            .expect("every region of the parent was cloned into the child")
    })
}

/// First code of a forked child's task. Runs with the child address space
/// active and interrupts enabled, like `trampoline`.
extern "C" fn fork_child_entry(frame: *mut c_void) {
    // Safety: `dispatch_sys_fork` created the box for this task alone.
    let frame: UserFrame = *unsafe { Box::from_raw(frame.cast::<UserFrame>()) };
    let process = Process::current();
    let populated = {
        let _exclusive = process.exclusive_user_memory();
        process
            .memory_regions()
            .populate_eager_copies(process.address_space())
    };
    if populated.is_err() {
        terminate_current(Signal::Kill);
    }
    // Safety: `frame` lives on this kernel stack until iretq.
    unsafe { resume_user_frame(&raw const frame) };
}
