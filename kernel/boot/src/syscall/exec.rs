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

    let args = ExecArgs::copy_in(argv_ptr, argc, envp_ptr, envc)?;

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

    let Ok((entry, rsp)) = setup_user_image(
        &process,
        task,
        &validated,
        &node,
        &args.argv(),
        &args.envp(),
    ) else {
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

const STACK_BYTES_PER_ARG: usize = 1 + size_of::<usize>();

struct ExecArgs {
    bytes: Vec<u8>,
    lens: Vec<usize>,
    argc: usize,
}

impl ExecArgs {
    fn copy_in(argv_ptr: usize, argc: usize, envp_ptr: usize, envc: usize) -> Result<Self, Errno> {
        let mut budget = 3 * size_of::<usize>();
        let mut bytes = vec![];
        let mut lens = vec![];

        for (ptr, count) in [(argv_ptr, argc), (envp_ptr, envc)] {
            if count == 0 {
                continue;
            }
            if ptr == 0 {
                return Err(EFAULT);
            }
            if budget.saturating_add(count.saturating_mul(STACK_BYTES_PER_ARG)) > ARG_MAX {
                return Err(E2BIG);
            }
            make_user_range_resident(ptr, count * size_of::<StrSlice>(), UserAccess::Read)?;

            for i in 0..count {
                let slot = unsafe { (ptr as *const StrSlice).add(i).read_unaligned() };
                budget = budget
                    .saturating_add(slot.len)
                    .saturating_add(STACK_BYTES_PER_ARG);
                if budget > ARG_MAX {
                    return Err(E2BIG);
                }
                if slot.len > 0 {
                    make_user_range_resident(slot.ptr, slot.len, UserAccess::Read)?;
                    let src = unsafe { slice_from_ptr_and_len::<u8>(slot.ptr, slot.len) }?;
                    if src.contains(&0) {
                        return Err(EINVAL);
                    }
                    bytes.extend_from_slice(src);
                }
                lens.push(slot.len);
            }
        }

        Ok(Self { bytes, lens, argc })
    }

    fn argv(&self) -> Vec<&[u8]> {
        self.slices(0, self.argc)
    }

    fn envp(&self) -> Vec<&[u8]> {
        self.slices(self.argc, self.lens.len() - self.argc)
    }

    fn slices(&self, skip: usize, take: usize) -> Vec<&[u8]> {
        let mut at: usize = self.lens[..skip].iter().sum();
        let mut out = Vec::with_capacity(take);
        for &len in &self.lens[skip..skip + take] {
            out.push(&self.bytes[at..at + len]);
            at += len;
        }
        out
    }
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
