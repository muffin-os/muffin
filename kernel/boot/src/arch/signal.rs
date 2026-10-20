use core::arch::asm;
use core::arch::x86_64::{_fxrstor64, _fxsave64};

use kernel_abi::{SigAction, SigSet, Signal};
use kernel_syscall::UserspaceMutPtr;
use kernel_syscall::signal::Disposition;
use tracing::info;
use x86_64::instructions::{hlt, interrupts};
use x86_64::registers::control::{Cr0, Cr0Flags};
use x86_64::registers::rflags::RFlags;
use x86_64::structures::idt::InterruptStackFrame;
use x86_64::{PrivilegeLevel, VirtAddr};

use crate::arch::gdt::Selectors;
use crate::arch::idt::SyscallRegisters;
use crate::mcore::mtask::process::{ExitOutcome, Process, Signals};
use crate::mcore::mtask::task::Task;
use crate::mcore::mtask::wait::try_reserve;

/// Marker written into every signal frame so `sigreturn` can reject a frame the
/// userspace program corrupted or forged.
pub const SIGFRAME_MAGIC: u64 = 0x8D2F_FAB8_19E3_EECC;

// The flags userspace owns, the same set Linux's FIX_EFLAGS lets sigreturn restore.
const USER_RFLAGS: RFlags = RFlags::CARRY_FLAG
    .union(RFlags::PARITY_FLAG)
    .union(RFlags::AUXILIARY_CARRY_FLAG)
    .union(RFlags::ZERO_FLAG)
    .union(RFlags::SIGN_FLAG)
    .union(RFlags::TRAP_FLAG)
    .union(RFlags::DIRECTION_FLAG)
    .union(RFlags::OVERFLOW_FLAG);

/// The full context saved on the user stack when a handler is entered. It is
/// read back verbatim by `sys_sigreturn` to restore the interrupted context.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
struct SigFrame {
    // First so the whole frame keeps the 16 byte alignment fxsave64 needs.
    // Callee saved GPRs need no slots because they survive the handler call chain.
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
fn capture_fpu(fx: &mut [u8; 512]) -> bool {
    // With CR0.TS clear the registers belong to this task. Otherwise the last
    // switch already spilled them into the task fx area. TS is left as is so
    // the lazy restore stays consistent with what the frame recorded.
    if !Cr0::read().contains(Cr0Flags::TASK_SWITCHED) {
        let mut buf = FxBuf([0u8; 512]);
        // Safety: buf is 16 byte aligned and 512 bytes, the size fxsave64 writes.
        unsafe { _fxsave64(buf.0.as_mut_ptr()) };
        fx.copy_from_slice(&buf.0);
        return true;
    }

    let task = Task::current();
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
/// Terminates the process on `Segfault` when the frame range is not mapped
/// user writable memory.
fn write_sigframe(
    frame: &InterruptStackFrame,
    regs: &SyscallRegisters,
    signo: Signal,
    old_blocked: SigSet,
    action: &SigAction,
) -> u64 {
    let user_rsp = frame.stack_pointer.as_u64();
    let size = size_of::<SigFrame>() as u64;
    // Skip the 128 byte red zone for ABI-conforming binaries (Muffin's own
    // toolchain disables it). The restorer goes at base - 8 so the handler
    // returns into it and enters with rsp % 16 == 8, as right after a call.
    let base = user_rsp.wrapping_sub(128).wrapping_sub(size) & !0xF;
    let start = base.wrapping_sub(8);

    let mut uptr = match UserspaceMutPtr::<u8>::try_from_usize(start as usize) {
        Ok(ptr) => ptr,
        Err(_) => terminate_current(Signal::Segfault),
    };
    let len = size_of::<SigFrame>() + 8;
    if uptr.validate_range(len).is_err() {
        terminate_current(Signal::Segfault);
    }
    // Delivery runs with interrupts off, so a lazily mapped page cannot be
    // paged in here and counts as unwritable.
    if !Process::current()
        .address_space()
        .is_user_writable(VirtAddr::new(start), len)
    {
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
        // Safety: the range start .. start + len is lower half, resident and
        // user writable, so these writes cannot fault.
        core::ptr::write_unaligned(dst.cast::<u64>(), action.restorer as u64);
        core::ptr::write_unaligned(dst.add(8).cast::<SigFrame>(), sf);
    }

    base
}

/// Reserves a lot slot for the current task and keeps the unpark ticket
/// in the signal state, so a later `Continue` or `Kill` can release it.
fn request_stop_park(task: &Task, signals: &mut Signals) {
    assert!(!interrupts::are_enabled());

    let Some(reservation) = try_reserve() else {
        return;
    };
    match task.set_park_reservation(reservation) {
        Ok(unpark_ticket) => signals.store_stop_unpark(unpark_ticket),
        // A pending ticket keeps its wake source, the fresh reservation goes
        // back to the lot.
        Err(reservation) => reservation.release(),
    }
}

/// Act on pending signals for the current task. A contended signals lock
/// delivers nothing, the signal stays pending until the next delivery attempt.
pub fn deliver_pending(frame: &mut InterruptStackFrame, regs: &mut SyscallRegisters) {
    debug_assert!(!interrupts::are_enabled());
    debug_assert_eq!(frame.code_segment.rpl(), PrivilegeLevel::Ring3);
    let task = Task::current();
    let process = task.process();
    let Some(mut guard) = process.try_signals_write() else {
        return;
    };
    loop {
        if guard.stopped() {
            request_stop_park(task, &mut guard);
            return;
        }
        let Some(signo) = guard.take_next_deliverable() else {
            return;
        };
        match guard.disposition(signo) {
            Disposition::Ignore => {}
            Disposition::DefaultStop => {
                guard.set_stopped(true);
                request_stop_park(task, &mut guard);
                return;
            }
            Disposition::DefaultTerminate => {
                drop(guard);
                // free the user allocations while this address space is active
                task.free_user_allocations();
                task.set_should_terminate(true);
                return;
            }
            Disposition::Handler(action) => {
                let old_blocked = guard.apply_handler_entry(signo, &action);
                drop(guard);
                let base = write_sigframe(frame, regs, signo, old_blocked, &action);
                regs.rdi = signo.number() as usize;
                unsafe {
                    // Safety: base - 8 is a validated user stack address and the
                    // handler address came from a checked sigaction.
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

pub fn reap_current_if_requested(task: &Task) -> bool {
    if !task.process().reap_requested_for(task.id()) {
        return false;
    }
    task.free_user_allocations();
    task.set_should_terminate(true);
    true
}

/// Restore the context a handler was entered from, reading the frame the
/// handler stack pointer points at. Terminates the process on `Segfault` when
/// the frame is not readable user memory or its magic does not match.
pub fn sys_sigreturn(frame: &mut InterruptStackFrame, regs: &mut SyscallRegisters) {
    let addr = frame.stack_pointer.as_u64() as usize;
    let ptr = match UserspaceMutPtr::<SigFrame>::try_from_usize(addr) {
        Ok(ptr) => ptr,
        Err(_) => terminate_current(Signal::Segfault),
    };
    if ptr.validate_range(size_of::<SigFrame>()).is_err() {
        terminate_current(Signal::Segfault);
    }

    if !Process::current()
        .address_space()
        .is_user_readable(frame.stack_pointer, size_of::<SigFrame>())
    {
        terminate_current(Signal::Segfault);
    }

    // Safety: lower half pointer, range validated and user readable, read by
    // value.
    let saved = unsafe { core::ptr::read_unaligned(ptr.as_ptr()) };
    if saved.magic != SIGFRAME_MAGIC {
        terminate_current(Signal::Segfault);
    }

    // Restores rax too, so sigreturn must not write its own result afterwards.
    *regs = saved.regs;

    let sel = Selectors::current();
    let rflags = (RFlags::from_bits_retain(saved.rflags) & USER_RFLAGS) | RFlags::INTERRUPT_FLAG;

    // Safety: rebuilding the interrupted user context. cs/ss come from the
    // trampoline selectors rather than the untrusted frame.
    unsafe {
        frame.as_mut().update(|f| {
            f.instruction_pointer = VirtAddr::new_truncate(saved.rip);
            f.stack_pointer = VirtAddr::new_truncate(saved.rsp);
            f.cpu_flags = rflags;
            f.code_segment = sel.user_code;
            f.stack_segment = sel.user_data;
        });
    }

    Process::current()
        .signals_write()
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
    // The fault path runs on the page fault IST stack. A context switch would
    // save the task's registers there, and the next page fault on this CPU
    // starts again at the fixed IST top, clobbering them. Interrupts stay off
    // until `Task::exit` marks the task terminated, because the scheduler
    // reaps a terminated task without saving its context.
    interrupts::disable();
    let process = Process::current();
    let pid = process.pid();
    process.set_exit_outcome(ExitOutcome::Signaled(signo));
    info!("terminating process on signal {} (pid {pid})", signo.name());
    Task::exit();
    loop {
        hlt();
    }
}

/// Record `signo` as pending on the current process and then terminate on it.
/// Used by fault paths so the bookkeeping reflects the signal that killed it.
pub fn force_fatal_current(signo: Signal) -> ! {
    Process::current().signals_write().set_pending(signo);
    terminate_current(signo);
}

/// Deliver a synchronous fault signal. A handler installed and not blocked runs
/// with the faulting instruction pointer preserved, so a returning handler
/// re-executes the faulting instruction. Otherwise the process dies, which is
/// the safe choice POSIX leaves undefined for ignored or blocked fault signals.
pub fn deliver_fault(frame: &mut InterruptStackFrame, regs: &mut SyscallRegisters, signo: Signal) {
    let process = Process::current().clone();
    let mut guard = process.signals_write();

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
