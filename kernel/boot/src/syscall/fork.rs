use alloc::borrow::ToOwned;
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
use crate::mcore::mtask::process::tree::process_tree;
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::Task;
use crate::mem::memapi::{LowerHalfAllocation, Writable};

/// POSIX fork. The child is a copy of the calling process with one task
/// that resumes at the parent's `int 0x80` return with rax = 0.
///
/// # Errors
/// `ENOMEM` when the regions cannot be cloned. `EAGAIN` when the child's
/// kernel stack cannot be allocated.
///
/// # Panics
/// Panics if a task handle of the caller has no counterpart in the child,
/// which a successful region clone rules out.
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

    let child = Process::create_new(&parent, parent.name().to_owned(), parent.executable_path());

    if parent
        .memory_regions()
        .clone_into(parent.address_space(), child.memory_regions(), &child.vmm())
        .is_err()
    {
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
    *child_task.tls().write() = counterpart(task.tls(), &child);
    *child_task.fx_area().write() = counterpart(task.fx_area(), &child);

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
    if process
        .memory_regions()
        .populate_eager_copies(process.address_space())
        .is_err()
    {
        terminate_current(Signal::Kill);
    }
    // Safety: `frame` lives on this kernel stack until iretq.
    unsafe { resume_user_frame(&raw const frame) };
}
