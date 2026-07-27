use core::arch::asm;
use core::arch::x86_64::{_fxrstor64, _fxsave64};

use kernel_abi::{SigAction, SigSet, Signal};
use kernel_syscall::UserspaceMutPtr;
use kernel_syscall::signal::Disposition;
use tracing::info;
use x86_64::VirtAddr;
use x86_64::instructions::{hlt, interrupts};
use x86_64::registers::control::{Cr0, Cr0Flags};
use x86_64::registers::rflags::RFlags;
use x86_64::structures::idt::InterruptStackFrame;

use crate::arch::idt::SyscallRegisters;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::ExitOutcome;
use crate::mcore::mtask::task::Task;

/// Marker written into every signal frame so `sigreturn` can reject a frame the
/// userspace program corrupted or forged.
pub const SIGFRAME_MAGIC: u64 = 0x8D2F_FAB8_19E3_EECC;

/// The full context saved on the user stack when a handler is entered. It is
/// read back verbatim by `sys_sigreturn` to restore the interrupted context.
///
/// The layout is fixed. `fx` lives at offset 0 so the whole frame keeps 16 byte
/// alignment for `fxsave64`/`fxrstor64`. Callee saved GPRs need no slots because
/// they survive the kernel C call chain, the handler, and the sigreturn chain.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
struct SigFrame {
    fx: [u8; 512],
    regs: SyscallRegisters,
    rip: u64,
    rflags: u64,
    rsp: u64,
    old_blocked: SigSet,
    signo: i64,
    fx_valid: bool,
    magic: u64,
}

/// A 16 byte aligned scratch buffer for the FPU save area. `fxsave64` and
/// `fxrstor64` fault on a misaligned pointer, so the kernel side copy of the
/// 512 byte image always lives in one of these.
#[repr(C, align(16))]
struct FxBuf([u8; 512]);

/// Capture the current FPU state into `fx`, returning `true` when `fx` holds real
/// state and `false` when the task has never touched the FPU.
///
/// Lazy FPU switching (CR0.TS) decides where the live state is. TS clear means
/// the registers belong to the current task, so they are saved directly. TS set
/// means the state was already spilled into the task fx area on the last switch,
/// so the frame copies that instead. The delivery path never clears TS. That
/// keeps the lazy restore consistent with what the frame recorded.
fn capture_fpu(fx: &mut [u8; 512]) -> bool {
    if !Cr0::read().contains(Cr0Flags::TASK_SWITCHED) {
        let mut buf = FxBuf([0u8; 512]);
        // Safety: buf is 16 byte aligned and 512 bytes, the size fxsave64 writes.
        unsafe { _fxsave64(buf.0.as_mut_ptr()) };
        fx.copy_from_slice(&buf.0);
        return true;
    }

    let task = ExecutionContext::load().current_task();
    let guard = task.fx_area().read();
    if let Some(fx_area) = guard.as_ref() {
        let src = fx_area.start().as_mut_ptr::<u8>();
        unsafe {
            // Safety: the fx area is a 512 byte FxArea allocation owned by the task,
            // and fx is a 512 byte buffer. The regions do not overlap.
            debug_assert_eq!(
                core::mem::align_of::<FxBuf>(),
                16,
                "must be 16 byte aligned"
            );
            core::ptr::copy_nonoverlapping(src, fx.as_mut_ptr(), 512)
        };
        true
    } else {
        false
    }
}

/// Build a signal frame on the user stack and return the frame base address.
///
/// The frame is placed below the red zone the interrupted code may still own,
/// then aligned down to 16 bytes. The restorer address is written at `base - 8`
/// so the handler returns into it, and the handler entry stack pointer becomes
/// `base - 8` which gives the `rsp % 16 == 8` shape the SysV ABI expects right
/// after a `call`.
// NOTE: user stack MUST be large enough to hold such a frame
fn write_sigframe(
    frame: &InterruptStackFrame,
    regs: &SyscallRegisters,
    signo: Signal,
    old_blocked: SigSet,
    action: &SigAction,
) -> u64 {
    let user_rsp = frame.stack_pointer.as_u64();
    let size = size_of::<SigFrame>() as u64;
    let base = user_rsp.wrapping_sub(128).wrapping_sub(size) & !0xF;
    let start = base - 8;

    let mut uptr = match unsafe { UserspaceMutPtr::<u8>::try_from_usize(start as usize) } {
        Ok(ptr) => ptr,
        Err(_) => terminate_current(Signal::Segfault),
    };
    if uptr.validate_range(size_of::<SigFrame>() + 8).is_err() {
        terminate_current(Signal::Segfault);
    }

    let mut sf = SigFrame {
        fx: [0u8; 512],
        regs: *regs,
        rip: frame.instruction_pointer.as_u64(),
        rflags: frame.cpu_flags.bits(),
        rsp: user_rsp,
        old_blocked,
        signo: signo.number() as i64,
        fx_valid: false,
        magic: SIGFRAME_MAGIC,
    };
    sf.fx_valid = capture_fpu(&mut sf.fx);

    let dst = uptr.as_mut_ptr();
    unsafe {
        // Safety: the range start .. start + size_of + 8 was validated as writable
        // lower half memory. The user stack is eagerly mapped by the trampoline.
        core::ptr::write_unaligned(dst.cast::<u64>(), action.restorer as u64);
        core::ptr::write_unaligned(dst.add(8).cast::<SigFrame>(), sf);
    }

    base
}

/// Deliver at most one pending signal at syscall exit and enforce default
/// dispositions. Runs the whole delivery decision loop until a handler is set
/// up (returns to run it), the process is terminated, or nothing is deliverable.
pub fn deliver_pending(frame: &mut InterruptStackFrame, regs: &mut SyscallRegisters) {
    let process = ExecutionContext::load().current_process().clone();

    loop {
        let mut guard = process.signals().write();
        let Some(signo) = guard.take_next_deliverable() else {
            return;
        };

        match guard.disposition(signo) {
            Disposition::Ignore => continue,
            Disposition::DefaultTerminate => {
                drop(guard);
                terminate_current(signo);
            }
            Disposition::DefaultStop => {
                guard.set_stopped(true);
                drop(guard);
                let pid = ExecutionContext::load().pid();
                info!("stopping process {pid} on signal {}", signo.name());
                // Interrupts are on in syscall context. The next timer tick
                // parks this task via the reschedule routing, and a SIGCONT or
                // SIGKILL clears the stopped flag to resume it here.
                //
                // The condition is evaluated with interrupts disabled so the
                // read guard can never span a context switch. A task parked
                // while holding this guard would wedge every later writer,
                // including the SIGCONT that is supposed to resume it.
                while interrupts::without_interrupts(|| process.signals().read().stopped()) {
                    hlt();
                }
                continue;
            }
            Disposition::Handler(action) => {
                let old_blocked = guard.apply_handler_entry(signo, &action);
                drop(guard);
                let base = write_sigframe(frame, regs, signo, old_blocked, &action);
                regs.rdi = signo.number() as usize;
                // Safety: redirecting the interrupted context into the handler is
                // the whole point of delivery. base - 8 is a validated user stack
                // address and the handler address came from a checked sigaction.
                unsafe {
                    frame.as_mut().update(|f| {
                        f.instruction_pointer =
                            VirtAddr::new_truncate(action.handler.addr() as u64);
                        f.stack_pointer = VirtAddr::new_truncate(base - 8);
                    });
                }
                return;
            }
        }
    }
}

/// Restore the context a handler was entered from. Reads the frame the handler
/// stack pointer points at, verifies the magic, and rebuilds the interrupted
/// registers, instruction pointer, stack pointer, flags, and FPU state.
pub fn sys_sigreturn(frame: &mut InterruptStackFrame, regs: &mut SyscallRegisters) {
    let addr = frame.stack_pointer.as_u64() as usize;
    let ptr = match unsafe { UserspaceMutPtr::<SigFrame>::try_from_usize(addr) } {
        Ok(ptr) => ptr,
        Err(_) => terminate_current(Signal::Segfault),
    };
    if ptr.validate_range(size_of::<SigFrame>()).is_err() {
        terminate_current(Signal::Segfault);
    }

    // Safety: range validated lower half pointer, read by value.
    let saved = unsafe { core::ptr::read_unaligned(ptr.as_ptr()) };
    if saved.magic != SIGFRAME_MAGIC {
        terminate_current(Signal::Segfault);
    }

    // Restores rax too, so sigreturn must not write its own result afterwards.
    *regs = saved.regs;

    let ctx = ExecutionContext::load();
    let user_code = ctx.selectors().user_code;
    let user_data = ctx.selectors().user_data;
    // Preserve the arithmetic flags the handler observed, force interrupts on,
    // and drop everything else the user must not control.
    let rflags = (saved.rflags & 0xDD5) | 0x200;

    // Safety: rebuilding the interrupted user context. cs/ss come from the
    // trampoline selectors rather than the untrusted frame.
    unsafe {
        frame.as_mut().update(|f| {
            f.instruction_pointer = VirtAddr::new_truncate(saved.rip);
            f.stack_pointer = VirtAddr::new_truncate(saved.rsp);
            f.cpu_flags = RFlags::from_bits_retain(rflags);
            f.code_segment = user_code;
            f.stack_segment = user_data;
        });
    }

    ctx.current_process()
        .signals()
        .write()
        .set_blocked_raw(saved.old_blocked);

    if saved.fx_valid {
        let mut buf = FxBuf([0u8; 512]);
        buf.0.copy_from_slice(&saved.fx);
        // Safety: buf is 16 byte aligned and holds a full fxsave image. clts
        // clears TS so fxrstor64 does not raise a device not available fault.
        unsafe {
            asm!("clts");
            _fxrstor64(buf.0.as_ptr());
        }
    }
}

/// Terminate the current process on `signo` and never return. Shared by default
/// terminate delivery, unhandled faults, and `SYS_EXIT`.
pub fn terminate_current(signo: Signal) -> ! {
    // The fault path arrives with interrupts disabled, and the exit hlt loop
    // needs the timer to eventually reap the task.
    if !interrupts::are_enabled() {
        interrupts::enable();
    }
    let ctx = ExecutionContext::load();
    let pid = ctx.pid();
    ctx.current_process()
        .set_exit_outcome(ExitOutcome::Signaled(signo));
    info!("terminating process on signal {} (pid {pid})", signo.name());
    Task::exit();
    loop {
        hlt();
    }
}

/// Record `signo` as pending on the current process and then terminate on it.
/// Used by fault paths so the bookkeeping reflects the signal that killed it.
pub fn force_fatal_current(signo: Signal) -> ! {
    ExecutionContext::load()
        .current_process()
        .signals()
        .write()
        .set_pending(signo);
    terminate_current(signo);
}

/// Deliver a synchronous fault signal. A handler installed and not blocked runs
/// with the faulting instruction pointer preserved, so a returning handler
/// re-executes the faulting instruction. Otherwise the process dies, which is
/// the safe choice POSIX leaves undefined for ignored or blocked fault signals.
pub fn deliver_fault(frame: &mut InterruptStackFrame, regs: &mut SyscallRegisters, signo: Signal) {
    let process = ExecutionContext::load().current_process().clone();
    let mut guard = process.signals().write();

    match guard.disposition(signo) {
        Disposition::Handler(action) if signo.bit() & guard.blocked() == 0 => {
            let old_blocked = guard.apply_handler_entry(signo, &action);
            drop(guard);
            let base = write_sigframe(frame, regs, signo, old_blocked, &action);
            regs.rdi = signo.number() as usize;
            // Safety: same as the handler path in deliver_pending.
            unsafe {
                frame.as_mut().update(|f| {
                    f.instruction_pointer = VirtAddr::new_truncate(action.handler.addr() as u64);
                    f.stack_pointer = VirtAddr::new_truncate(base - 8);
                });
            }
        }
        _ => {
            drop(guard);
            force_fatal_current(signo);
        }
    }
}
