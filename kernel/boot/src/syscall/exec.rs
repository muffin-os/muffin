use alloc::borrow::ToOwned;
use alloc::vec;
use alloc::vec::Vec;

use kernel_abi::{
    ARG_MAX, E2BIG, EFAULT, EINVAL, EIO, ENAMETOOLONG, ENOENT, ENOEXEC, ENOMEM, Errno, PATH_MAX,
    Signal, StrSlice,
};
use kernel_vfs::path::{AbsoluteOwnedPath, AbsolutePath, Path};
use x86_64::VirtAddr;
use x86_64::registers::model_specific::FsBase;
use x86_64::registers::rflags::RFlags;
use x86_64::structures::idt::InterruptStackFrame;

use super::{UserAccess, make_user_range_resident, slice_from_ptr_and_len};
use crate::arch::idt::SyscallRegisters;
use crate::arch::signal::terminate_current;
use crate::file::vfs;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::elf::{self, LoadExecutableError};
use crate::mcore::mtask::process::setup_user_image;

/// POSIX execve. Replaces the calling process's image while preserving pid,
/// ppid, cwd, open file descriptors, the signal mask, and pending signals.
///
/// Every fallible check runs before the old image is torn down, so an error
/// return leaves the caller intact. Once teardown starts, a failure kills the
/// process, because there is no image left to return into.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_sys_execve(
    path_ptr: usize,
    path_len: usize,
    argv_ptr: usize,
    argc: usize,
    envp_ptr: usize,
    envc: usize,
    frame: &mut InterruptStackFrame,
    regs: &mut SyscallRegisters,
) -> Result<usize, Errno> {
    let path = copy_in_path(path_ptr, path_len)?;

    let mut budget = 3 * size_of::<usize>();
    let argv = copy_in_str_array(argv_ptr, argc, &mut budget)?;
    let envp = copy_in_str_array(envp_ptr, envc, &mut budget)?;

    let node = vfs().write().open(&path).map_err(|_| ENOENT)?;
    let validated = elf::validate(&node).map_err(exec_errno)?;

    let ctx = ExecutionContext::load();
    let process = ctx.current_process().clone();
    let task = ctx.current_task();

    let sole = process.reap_sibling_tasks(task.id());

    task.free_user_allocations();
    FsBase::write(VirtAddr::zero());
    process.executable_segments().write().clear();
    process
        .memory_regions()
        .clear(process.address_space(), &sole);

    let argv_refs: Vec<&[u8]> = argv.iter().map(Vec::as_slice).collect();
    let envp_refs: Vec<&[u8]> = envp.iter().map(Vec::as_slice).collect();
    let Ok((entry, rsp)) =
        setup_user_image(&process, task, &validated, &node, &argv_refs, &envp_refs)
    else {
        terminate_current(Signal::Kill);
    };

    process.set_executable_path(path);
    process.finish_reap();
    process.signals_write().exec_reset();

    let sel = ctx.selectors();
    // Safety: building a Ring 3 entry frame for the new image. cs and ss come
    // from the trampoline selectors rather than any user-controlled value.
    unsafe {
        frame.as_mut().update(|f| {
            f.instruction_pointer = entry;
            f.stack_pointer = rsp;
            f.cpu_flags = RFlags::INTERRUPT_FLAG;
            f.code_segment = sel.user_code;
            f.stack_segment = sel.user_data;
        });
    }
    // The new image must observe no register value from the old one. The
    // dispatcher's rax result write lands on this zeroed block.
    *regs = SyscallRegisters::default();
    Ok(0)
}

fn copy_in_path(ptr: usize, len: usize) -> Result<AbsoluteOwnedPath, Errno> {
    if len > PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    make_user_range_resident(ptr, len, UserAccess::Read)?;
    let bytes = unsafe { slice_from_ptr_and_len::<u8>(ptr, len) }?;
    let s = core::str::from_utf8(bytes).map_err(|_| EINVAL)?;
    let rel = Path::new(s);
    if let Ok(p) = AbsolutePath::try_new(rel) {
        Ok(p.to_owned())
    } else {
        let mut p = ExecutionContext::load()
            .current_process()
            .current_working_directory()
            .read()
            .clone();
        p.push(rel);
        Ok(p)
    }
}

/// All user memory becomes resident here, before any filesystem lock is
/// taken, per the invariant on [`make_user_range_resident`].
fn copy_in_str_array(ptr: usize, count: usize, budget: &mut usize) -> Result<Vec<Vec<u8>>, Errno> {
    if count == 0 {
        return Ok(vec![]);
    }
    if ptr == 0 {
        return Err(EFAULT);
    }
    let array_len = count.checked_mul(size_of::<StrSlice>()).ok_or(E2BIG)?;
    make_user_range_resident(ptr, array_len, UserAccess::Read)?;

    let mut strings = Vec::with_capacity(count);
    for i in 0..count {
        // Safety: the array range is resident and user readable, and the
        // read tolerates an unaligned element address.
        let slot = unsafe { (ptr as *const StrSlice).add(i).read_unaligned() };
        let charge = slot.len.checked_add(1 + size_of::<usize>()).ok_or(E2BIG)?;
        *budget = budget.checked_add(charge).ok_or(E2BIG)?;
        if *budget > ARG_MAX {
            return Err(E2BIG);
        }
        let bytes = if slot.len == 0 {
            vec![]
        } else {
            make_user_range_resident(slot.ptr, slot.len, UserAccess::Read)?;
            unsafe { slice_from_ptr_and_len::<u8>(slot.ptr, slot.len) }?.to_vec()
        };
        // An embedded NUL would truncate the C string written onto the new
        // image's initial stack.
        if bytes.contains(&0) {
            return Err(EINVAL);
        }
        strings.push(bytes);
    }
    Ok(strings)
}

fn exec_errno(e: LoadExecutableError) -> Errno {
    match e {
        LoadExecutableError::Stat | LoadExecutableError::Read => EIO,
        LoadExecutableError::AllocationFailed => ENOMEM,
        LoadExecutableError::Truncated
        | LoadExecutableError::Parse(_)
        | LoadExecutableError::UnsupportedType(_)
        | LoadExecutableError::NotPageCongruent { .. }
        | LoadExecutableError::WritableExecutable(_)
        | LoadExecutableError::InvalidVirtualAddress(_)
        | LoadExecutableError::InvalidSizeOrAlign
        | LoadExecutableError::FileLongerThanMemory { .. }
        | LoadExecutableError::AlreadyReserved(_)
        | LoadExecutableError::TooManyTlsHeaders => ENOEXEC,
    }
}
