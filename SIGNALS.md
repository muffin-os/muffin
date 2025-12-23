# Muffin OS Signal Handling Design

This document describes a concrete, implementable design for POSIX-like signal handling in Muffin OS. It is written to be directly actionable, but *tailored to Muffin’s current process/task model* (`kernel/src/mcore/mtask`) and x86_64 entrypoints (`kernel/src/arch/idt.rs`).

Status (as of 2025-12-23):
- No signal subsystem is implemented yet.
- `kernel_abi` reserves `SYS_SIGNAL`, but the kernel does not dispatch it.
- Faults that should become `SIGSEGV` currently terminate the current task (`Task::set_should_terminate(true)`) or panic (see `kernel/src/arch/idt.rs`).

Scope assumptions in Muffin terms:
- **Task** (`crate::mcore::mtask::task::Task`) is the schedulable entity.
- **Process** (`crate::mcore::mtask::process::Process`) owns the address space / file descriptors and is referenced by tasks via `Arc<Process>`.
- User-mode transitions happen via `iretq` (notably from `Process::trampoline` and the `int 0x80` syscall handler).
- Syscalls are dispatched by `kernel/src/syscall/mod.rs` and return `kernel_abi::Errno` values.

Where this document uses POSIX terminology, the mapping to Muffin types is described in §1.

---

## 0. Goals and non-goals

### Goals
- POSIX-like signal semantics for:
  - standard (non-RT) signals: coalescing pending instances
  - signal masks per thread, process-directed vs thread-directed delivery
  - `sigaction` handlers and default dispositions
  - syscall interruption (`EINTR`) and optional `SA_RESTART`
  - stop/continue semantics (job control core)
  - `waitpid()` interaction: termination, stop/continue state reporting
- A design that is safe (no handler execution in interrupt context), race-resilient, and debuggable.

### Non-goals (for first milestone)
- Full POSIX job control / controlling TTY semantics (can be added later)
- Realtime signals (`SIGRTMIN..`) and `sigqueue()` (planned, but not in initial cut)
- Alternative signal stack (`sigaltstack`) (planned)
- Core dump production (default action can mark “coredump requested” without implementing dumping yet)

---

## 1. Terminology and invariants

- **Task**: schedulable entity with CPU register state (`crate::mcore::mtask::task::Task`).
- **Process**: address space + shared resources (files, VM) (`crate::mcore::mtask::process::Process`).
- **Process-directed signal**: addressed to a process; delivered to *one eligible task* in that process.
- **Task-directed signal**: addressed to a specific task; delivered to that task only.
- **Pending signal**: queued for later delivery.

Terminology note: the rest of this document uses classic POSIX wording like “thread” and “thread-directed”. In Muffin, read those as **task** and **task-directed** respectively.

Key invariants:
1. **Signals are delivered only at safe points** (never directly from interrupt context):
   - on return to user mode
   - on syscall exit
   - at explicit scheduler preemption points (if your kernel supports preempting in kernel mode; otherwise omit)
2. **Signal handler code runs in user mode**.
3. **All signal state mutations are synchronized** via per-task locks or atomic bit operations.

---

## 2. Data model

Assume `NSIG` (e.g., 64) and represent sets as bitsets.

### 2.1 Core types

```rust
// Conceptual Rust-side ABI/types. You will likely want to put most of these in `kernel_abi`
// and keep kernel-internal state in `kernel/src/...`.

pub const NSIG: usize = 64;

/// Bitset of pending/blocked standard (non-RT) signals.
/// Convention: bit (signo - 1) corresponds to signal number `signo` in 1..=NSIG.
pub type SigSet = u64;

pub type SignalNumber = u8;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SigInfo {
    pub signo: i32,
    pub code: i32,
    pub errno: i32,

    pub pid: u64,
    pub uid: u64,

    pub status: i32, // for SIGCHLD
    pub addr: usize, // for SIGSEGV
}

/// Userspace handler pointer. Convention:
/// - 0 = SIG_DFL
/// - 1 = SIG_IGN
/// - otherwise: userspace function address
pub type SigHandler = usize;

pub type SaFlags = u32;
pub const SA_RESTART: SaFlags = 1 << 0;
pub const SA_SIGINFO: SaFlags = 1 << 1;
pub const SA_NODEFER: SaFlags = 1 << 2;
pub const SA_RESETHAND: SaFlags = 1 << 3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SigAction {
    pub handler: SigHandler,
    pub mask: SigSet,
    pub flags: SaFlags,
}

// --- kernel-internal state ---
//
// These structs do NOT carry their own locks.
// In Rust, you typically make the lock the *owner* of the state:
//
//   struct Task { sig: spin::Mutex<TaskSignalState>, ... }
//   struct Process { sig: spin::RwLock<ProcessSignalState>, ... }
//
// That makes it unambiguous what is synchronized, and it prevents accidentally accessing
// partially-updated state without holding the guard.

pub struct TaskSignalState {
    pub blocked: SigSet,
    pub pending: SigSet,

    // In Muffin you may want an AtomicBool fast-path (set from interrupt context),
    // and re-check under the `sig` lock before clearing it.
    pub signal_pending_flag: bool,

    // user-mode signal delivery support (conceptual pointers)
    pub sig_trampoline: Option<usize>,
    pub altstack_base: Option<usize>,
    pub altstack_size: usize,
    pub altstack_enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Stopped,
    Zombie,
}

pub struct ProcessSignalState {
    pub actions: [SigAction; NSIG],
    pub pending: SigSet,

    pub state: ProcessState,
    pub exit_code: i32,
    pub term_signal: Option<SignalNumber>,
}
```

### 2.4 Why separate per-thread vs per-process pending?
- POSIX requires process-directed signals to be deliverable to *any one unblocked thread*.
- Thread-directed signals must not “move” to another thread.
- Keeping two pending sets simplifies selection logic and avoids races.

---

## 3. Default dispositions table

Define a table mapping each signal to default action:
- **TERM**: terminate process
- **CORE**: terminate + mark core dump
- **IGNORE**: drop signal
- **STOP**: stop process
- **CONT**: continue process

Example (partial):
- SIGKILL: TERM (unblockable, uncatchable)
- SIGSTOP: STOP (unblockable, uncatchable)
- SIGCONT: CONT (catchable/ignorable per POSIX-ish)
- SIGCHLD: IGNORE by default (but wait semantics still apply)

Enforcement rules:
- SIGKILL and SIGSTOP must not be blocked and must not have custom handlers.

---

## 4. Kernel entry points / syscalls (Muffin)

Muffin’s syscall numbers live in `kernel/crates/kernel_abi/src/syscall.rs` and are dispatched by `kernel/src/syscall/mod.rs` from the x86_64 `int 0x80` handler (`kernel/src/arch/idt.rs::syscall_handler_impl`).

`kernel_abi` already defines `SYS_SIGNAL = 25`. The simplest integration is to treat this as a **signal multiplexer** (so we don’t need to allocate many new syscall numbers up front):

```rust
// conceptual ABI (multiplexed via `kernel_abi::SYS_SIGNAL`)
fn sys_signal(
    op: u32,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
) -> Result<usize, kernel_abi::Errno>;
```

with an `op` enum in `kernel_abi` (e.g. `SignalOp::{Sigaction,Sigprocmask,Sigpending,Sigsuspend,Kill,Sigreturn}`), and a `dispatch_sys_signal(...)` arm added to `kernel/src/syscall/mod.rs`.

If you prefer Linux-style separate syscalls, allocate additional `SYS_*` numbers instead; the rest of this document is independent of that choice.

Implement these syscalls first:

1. `sys_sigaction(signo, act, oldact)`

   ```rust
   fn sys_sigaction(
       signo: SignalNumber,
       act: Option<kernel_syscall::UserspacePtr<SigAction>>,
       oldact: Option<kernel_syscall::UserspaceMutPtr<SigAction>>,
   ) -> Result<usize, kernel_abi::Errno>;
   ```

   - validate `signo` range and disallow changing SIGKILL/SIGSTOP
   - copy `act` from userspace, store into `process.actions[signo]`

2. `sys_sigprocmask(how, set, oldset)`

   ```rust
   fn sys_sigprocmask(
       how: i32,
       set: Option<kernel_syscall::UserspacePtr<SigSet>>,
       oldset: Option<kernel_syscall::UserspaceMutPtr<SigSet>>,
   ) -> Result<usize, kernel_abi::Errno>;
   ```

   - per-task mask
   - ensure SIGKILL/SIGSTOP bits are always cleared in the resulting mask

3. `sys_sigpending(set)`

   ```rust
   fn sys_sigpending(set: kernel_syscall::UserspaceMutPtr<SigSet>) -> Result<usize, kernel_abi::Errno>;
   ```

   - returns union of task and process pending (standard signals)

4. `sys_sigsuspend(mask)`

   ```rust
   fn sys_sigsuspend(mask: kernel_syscall::UserspacePtr<SigSet>) -> Result<usize, kernel_abi::Errno>;
   ```

   - atomically swap mask, sleep until a signal is delivered, then restore old mask and return `-EINTR`

5. Signal sending:

   ```rust
   fn sys_kill(pid: u64, signo: SignalNumber) -> Result<usize, kernel_abi::Errno>;
   // later (task-directed)
   fn sys_tgkill(tgid: u64, tid: u64, signo: SignalNumber) -> Result<usize, kernel_abi::Errno>;
   ```

6. Return from handler:

   ```rust
   fn sys_sigreturn() -> Result<usize, kernel_abi::Errno>;
   ```

   - used by the userspace trampoline to restore saved user context/registers

Design decision: use an explicit `sigreturn` syscall rather than relying on architecture-specific magic. This keeps the ABI clear and makes auditing easier.

---

## 5. Signal generation and queuing

### 5.1 Queueing from kernel code
Provide internal APIs:

```rust
// process-directed
fn send_signal_process(
    p: &alloc::sync::Arc<crate::mcore::mtask::process::Process>,
    signo: SignalNumber,
    info: Option<SigInfo>,
) -> Result<(), kernel_abi::Errno>;

// task-directed
fn send_signal_task(
    t: &crate::mcore::mtask::task::Task,
    signo: SignalNumber,
    info: Option<SigInfo>,
) -> Result<(), kernel_abi::Errno>;
```

Rules for standard signals (non-RT):
- Pending is a bitset; multiple sends coalesce into one pending bit.
- For `SigInfo`, store a single “last info” per signal if desired, but POSIX standard signals don’t guarantee queueing. Minimal: store none, only signo.

Important: these functions must be callable from interrupt context.
- Therefore they must not sleep.
- They must only set pending bits and mark `signal_pending_flag` and possibly wake a blocked thread/process.

### 5.2 Waking semantics
When setting pending:
- If process-directed: wake at least one thread in the process that could take the signal (or just wake all threads in the process in the simplest implementation).
- If thread-directed: wake that thread.

Recommended starting point (simpler):
- Wake all threads in target process when process.pending changes.
- Optimize later to “wake one eligible thread” after correctness is proven.

---

## 6. Delivery model (the heart): `do_signal()`

Delivery happens at safe points.

### 6.1 Safe points wiring (Muffin)
In Muffin, “return to user” currently happens via `iretq` in a few concrete places:

1. **Syscall exit**: `kernel/src/arch/idt.rs::syscall_handler_impl` runs with access to:
   - the `InterruptStackFrame` (RIP/CS/RFLAGS/RSP/SS), and
   - the pushed syscall GPR set (`SyscallRegisters`).

   This is the primary safe point: after `dispatch_syscall(...)` returns, but before the wrapper `iretq`, call `maybe_deliver_signals(current_task, user_context)`.

2. **Initial user entry**: `kernel/src/mcore/mtask/process/mod.rs::trampoline` constructs an `InterruptStackFrameValue` and calls `iretq`. If you support “signals pending before first user instruction”, check and deliver right before that final `iretq`.

If later you add additional user-return paths (e.g. an assembly context switch that returns to ring3), those must also call into signal delivery.

### 6.2 Selecting the next signal
Algorithm:
1. Compute deliverable set:
   - `pending_all = t.pending | t.process->pending`
   - `deliverable = pending_all & ~t.blocked`
   - Always treat SIGKILL and SIGSTOP as deliverable even if mask says blocked (but we also enforce clearing them from mask).
2. If none: clear `signal_pending_flag` and return.
3. Choose the lowest-numbered signal (traditional) or implement a fixed priority order. Consistency matters more than which, for standard signals.

### 6.3 Consume pending source
If chosen signal is thread-pending: clear bit in `t.pending`.
Else (process-pending): clear bit in `p.pending`.

Important concurrency note:
- Hold `t.sig.lock` and `p.sig.lock` as needed.
- To avoid deadlocks, define lock order: **process lock first, then thread lock**, or vice versa, and follow consistently.

### 6.4 Determine disposition
Disposition resolution:
- If `signo` is SIGKILL: force terminate.
- If `signo` is SIGSTOP: force stop.
- Else read `p.actions[signo]`:
  - handler == SIG_IGN => ignore (but see SIGCHLD nuance)
  - handler == SIG_DFL => default action from table
  - else => user handler

### 6.5 Apply default actions
Implement in kernel:

**IGNORE**:
- drop and continue; loop to check if more deliverable signals.

**TERM/CORE**:
- mark process as dying, set `term_signal`, set exit status
- transition to ZOMBIE, wake parent waiter
- tear down threads (or mark for exit) according to your kernel model

**STOP**:
- set process state STOPPED
- stop scheduling threads in the process
- notify parent via wait status (WIFSTOPPED)

**CONT**:
- if currently STOPPED, transition to RUNNING
- ensure threads are runnable
- notify parent via SIGCHLD if you support it

Design decision: STOP/CONT are handled in-kernel to avoid relying on user handlers for job control semantics.

### 6.6 Delivering a user handler
To invoke a handler:
1. Prepare a **signal frame** on the user stack (or altstack later).
2. Save full user register context + signal mask + (optional) siginfo and ucontext.
3. Update thread mask:
   - `new_mask = t.blocked | (1u64 << (signo - 1))` unless `SA_NODEFER` is set
   - also OR `sa_mask`
4. Redirect user PC to handler entry.
5. Set up return address / trampoline so handler can call `sigreturn`.

#### Frame layout
Define a stable ABI struct:

```rust
#[repr(C)]
pub struct SigFrame {
    pub magic: u64,
    pub signo: SignalNumber,
    pub _pad: [u8; 7],

    pub old_mask: SigSet,

    // Architecture-defined saved user context (registers, etc.).
    pub ucontext: UContext,

    // Only valid/used when SA_SIGINFO is set.
    pub info: SigInfo,
}
```

Implementation steps:
- Compute user stack pointer `usp`.
- If `SA_ONSTACK` later and altstack enabled, switch `usp`.
- Decrement `usp` by `core::mem::size_of::<SigFrame>()`, align to 16 bytes.
- `copyout()` frame to user memory.
- Modify saved user regs in the trapframe:
  - set instruction pointer to handler
  - set first arg register to `signo`
  - if `SA_SIGINFO`, pass pointers to `info` and `ucontext` within the frame
  - set stack pointer to new `usp`
  - set return address / link register to trampoline

Trampoline:
- Provide a small userspace stub at a known address:
  - either mapped “vdso-like” page or provided via libc
  - stub calls `sigreturn()` syscall

Design decision: require a trampoline address in `task.sig_trampoline` for early versions, later add a kernel-provided vdso mapping to remove libc dependence.

---

## 7. Syscall interruption and restart

Signals interact with blocking syscalls.

### 7.1 Kernel rule
If a thread is in an interruptible sleep (e.g., waiting on IO) and a signal becomes pending and deliverable:
- wake it
- syscall should return -EINTR unless `SA_RESTART` applies and the syscall is restartable

### 7.2 Implementation mechanism
Add to thread sleep primitives:
- `sleep_interruptible(waitq)` checks `signal_pending_deliverable(current)` before sleeping and after wake
- if deliverable: abort sleep with `-EINTR`

Restart:
- Tag each syscall as restartable or not.
- On signal delivery, if handler has `SA_RESTART` and syscall is restartable, arrange to retry the syscall on return (per-arch: set PC back to syscall instruction or store a restart token).

Milestone suggestion: implement -EINTR first, then add `SA_RESTART` once basic delivery is stable.

---

## 8. `waitpid()` and state reporting

Note: Muffin does not currently expose POSIX `waitpid()`/zombie semantics in its syscall surface. Keep this section as the target design, but expect to stage it behind the introduction of process exit/cleanup APIs (today, tasks terminate via `Task::set_should_terminate(true)` and `Task::exit()` loops on `hlt()`).

To be POSIX-like, parent wait must observe:
- normal exit
- termination by signal
- stop/continue transitions (if you implement job control)

Process struct additions:
- `exit_code`, `term_signal`, `state`
- condition variable or wait queue for parent

When child changes state:
- wake parent waiters
- optionally queue SIGCHLD to parent (process-directed)

Status encoding:
- Define macros similar to `WIFEXITED`, `WIFSIGNALED`, `WIFSTOPPED` for userland.

---

## 9. Concurrency and locking

### 9.1 Locking rules
- `Process.sig: spin::RwLock<ProcessSignalState>` (or `Mutex`) protects `pending`, `actions[]`, and process signal-related state transitions.
- `Task.sig: spin::Mutex<TaskSignalState>` (or `RwLock`) protects `pending`, `blocked`, and `signal_pending_flag`.

Lock ordering (choose one and document it in code):
- Recommended: **process lock → thread lock**

### 9.2 Atomic fast path
For performance, you may implement `signal_pending_flag` as an atomic and set it whenever:
- any pending bit is set in thread or process

Clearing must be careful:
- only clear when you re-check and find no deliverable pending.

---

## 10. Step-by-step implementation plan (concrete milestones)

### Milestone 1: Minimal standard signals (terminate/ignore)
1. Add `SigSet`, `SigAction`, and per-task/per-process signal state structs.
2. Initialize defaults in process creation:
   - all actions default
   - pending sets empty
3. Implement `sys_sigaction`, `sys_sigprocmask`, `sys_sigpending`.
4. Implement `sys_kill` that sets `process.pending`.
5. Add safe-point hook calling `maybe_deliver_signals()` on return to user.
6. Implement `do_signal()` with:
   - selection from bitsets
   - default ignore/terminate
   - disallow catching/blocking SIGKILL/SIGSTOP
7. Validate: sending SIGTERM kills a process; SIGUSR1 ignored/handled if handler installed.

### Milestone 2: User handlers + `sigreturn`
1. Define `SigFrame` ABI and per-arch ucontext save/restore.
2. Implement building the signal frame and redirecting PC/SP.
3. Implement `sys_sigreturn` to restore registers + mask.
4. Implement `SA_NODEFER`, `SA_RESETHAND`.

### Milestone 3: Blocking syscalls and EINTR
1. Modify interruptible sleeps to return -EINTR if deliverable signal pending.
2. Ensure `sigsuspend()` works (swap mask + sleep + restore).

### Milestone 4: STOP/CONT + wait status
1. Implement STOP/CONT default actions in-kernel.
2. Extend process state machine and scheduler integration for STOPPED.
3. Update `waitpid()` to report stop/continue.

### Milestone 5: Multi-thread correctness (if applicable)
1. Add process-directed delivery to “one eligible thread”:
   - choose a runnable thread with signal unblocked
   - if none, keep pending in process until one becomes eligible
2. Add `tgkill()` for thread-directed signals.

### Milestone 6: Realtime signals and `sigqueue` (optional)
1. Replace per-signal bit-only storage with queued nodes for RT signals.
2. Implement ordering and `siginfo` delivery guarantees.

---

## 11. Edge cases and required semantics

- **Exec**: handlers reset to default on `exec()` except those set to SIG_IGN remain ignored (POSIX). Masks typically preserved per-thread.
- **Fork**: child inherits actions and mask; pending signals are typically cleared in child (match POSIX/Linux behavior).
- **SIGCHLD**: if ignored, child may be auto-reaped depending on your POSIX level; simplest: still create zombies and require wait, add optimization later.
- **Unblock race**: when a thread unblocks a signal, it must promptly notice pending process signals (set `signal_pending_flag`).

---

## 12. Test checklist (developer-facing)

1. Handler invocation:
   - install handler for SIGUSR1; send signal; verify handler ran and process resumes.
2. Masking:
   - block SIGUSR1; send; verify not delivered until unblocked.
3. Coalescing:
   - send SIGUSR1 N times while blocked; unblock; verify handler runs once (standard signals).
4. SIGKILL invariants:
   - attempt to block or catch SIGKILL; ensure kernel rejects or ignores changes.
5. EINTR:
   - block in interruptible read/sleep; send signal; verify syscall returns -EINTR.
6. STOP/CONT:
   - send SIGSTOP; verify `waitpid` reports stopped; send SIGCONT; verify resumed.

---

## 13. Rationale summary

- **Safe-point delivery** avoids executing user handlers in interrupt context and simplifies locking.
- **Split pending sets (thread vs process)** matches POSIX delivery rules and keeps selection predictable.
- **Bitset pending for standard signals** matches coalescing semantics and is efficient.
- **Explicit sigreturn syscall + structured frame** provides a clean, portable ABI and simplifies debugging.

---

## Appendix A: Pseudocode

### A.1 `maybe_deliver_signals()`
```rust
fn maybe_deliver_signals(t: &crate::mcore::mtask::task::Task /*, user_context: &mut UserContext */) {
    loop {
        let signo = pick_next_deliverable_signal(t);
        let Some(signo) = signo else {
            // clear fast-path flag (after re-checking under lock)
            // t.sig.signal_pending_flag.store(false, Relaxed);
            return;
        };

        let disp = resolve_disposition(t.process(), signo);
        match disp {
            Disposition::Ignore => continue,
            Disposition::Terminate => terminate_process(t.process(), signo),
            Disposition::Stop => {
                stop_process(t.process(), signo);
                return;
            }
            Disposition::Continue => {
                continue_process(t.process(), signo);
                continue;
            }
            Disposition::Handler => {
                setup_user_signal_frame(t, signo /*, user_context */);
                return; // deliver one at a time
            }
        }
    }
}
```

### A.2 Picking signal
```rust
fn pick_next_deliverable_signal(t: &crate::mcore::mtask::task::Task) -> Option<SignalNumber> {
    let pending: SigSet = t.sig.pending | t.process().sig.pending;
    let deliverable: SigSet = pending & !t.sig.blocked;
    if deliverable == 0 {
        return None;
    }

    // Choose lowest-numbered set bit.
    let bit = deliverable.trailing_zeros() as u8;
    Some(bit + 1)
}
```
