#![no_std]

use core::arch::asm;
use core::arch::x86_64::_mm_pause;
use core::ffi::c_int;
use core::sync::atomic::{AtomicBool, Ordering};

pub use kernel_abi::{
    CLOCK_MONOTONIC, DefaultAction, FbScreenInfo, IoctlRequest, MapFlags, ProtFlags, SaFlags,
    SigAction, SigHandler, SigMaskHow, SigSet, Signal, Timespec,
};
use linked_list_allocator::LockedHeap;

pub fn exit(code: i32) -> ! {
    syscall1(1, code as usize);
    loop {
        _mm_pause();
    }
}

pub fn read(fd: c_int, buf: &mut [u8]) -> c_int {
    syscall3(36, fd as usize, buf.as_mut_ptr() as usize, buf.len()) as i32
}

pub fn write(fd: c_int, buf: &[u8]) -> c_int {
    syscall3(37, fd as usize, buf.as_ptr() as usize, buf.len()) as i32
}

pub fn ioctl<T>(fd: c_int, request: IoctlRequest, arg: &mut T) -> c_int {
    syscall3(
        48,
        fd as usize,
        request.number(),
        core::ptr::from_mut(arg) as usize,
    ) as c_int
}

pub fn fsync(fd: c_int) -> c_int {
    syscall1(49, fd as usize) as c_int
}

pub fn clock_gettime(clockid: usize, tp: &mut Timespec) -> c_int {
    syscall2(50, clockid, tp as *mut Timespec as usize) as c_int
}

#[unsafe(naked)]
pub extern "C" fn sigreturn_restorer() {
    core::arch::naked_asm!("mov rax, 46", "int 0x80")
}

pub fn kill(pid: i64, signo: Signal) -> c_int {
    syscall2(42, pid as usize, signo.number() as usize) as c_int
}

pub fn sigaction(signo: Signal, new: Option<&SigAction>, old: Option<&mut SigAction>) -> c_int {
    let new_ptr = new.map_or(0, |a| a as *const SigAction as usize);
    let old_ptr = old.map_or(0, |a| a as *mut SigAction as usize);
    syscall3(43, signo.number() as usize, new_ptr, old_ptr) as c_int
}

pub fn sigprocmask(how: SigMaskHow, set: Option<&SigSet>, old: Option<&mut SigSet>) -> c_int {
    let set_ptr = set.map_or(0, |s| s as *const SigSet as usize);
    let old_ptr = old.map_or(0, |s| s as *mut SigSet as usize);
    syscall3(44, how as usize, set_ptr, old_ptr) as c_int
}

pub fn sigpending(out: &mut SigSet) -> c_int {
    syscall1(45, out as *mut SigSet as usize) as c_int
}

pub fn getpid() -> i64 {
    syscall0(47) as i64
}

pub fn install_handler(signo: Signal, handler: extern "C" fn(Signal)) -> c_int {
    let action = SigAction {
        handler: SigHandler::new(handler as usize),
        mask: 0,
        flags: SaFlags::default(),
        restorer: sigreturn_restorer as *const () as usize,
    };
    sigaction(signo, Some(&action), None)
}

pub fn syscall0(n: usize) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            options(nostack),
        );
    }
    result
}

pub fn syscall1(n: usize, arg1: usize) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            options(nostack),
        );
    }
    result
}

pub fn syscall2(n: usize, arg1: usize, arg2: usize) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            in("rsi") arg2,
            options(nostack),
        );
    }
    result
}

pub fn syscall3(n: usize, arg1: usize, arg2: usize, arg3: usize) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            options(nostack),
        );
    }
    result
}

pub fn syscall6(
    n: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> usize {
    let result;
    unsafe {
        // Safety: traps into the kernel via int 0x80. Only the declared ABI
        // registers are read and the kernel preserves every register except rax.
        asm!(
            "int 0x80",
            inlateout("rax") n => result,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("rcx") arg4,
            in("r8") arg5,
            in("r9") arg6,
            options(nostack),
        );
    }
    result
}

pub fn mmap(
    addr: usize,
    len: usize,
    prot: ProtFlags,
    flags: MapFlags,
    fd: usize,
    offset: usize,
) -> isize {
    syscall6(
        41,
        addr,
        len,
        prot.bits() as usize,
        flags.bits() as usize,
        fd,
        offset,
    ) as isize
}

pub fn open(path: &str) -> c_int {
    syscall6(3, path.as_ptr() as usize, path.len(), 0, 0, 0, 0) as c_int
}

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

static HEAP_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Sets up the process heap so the global allocator can serve allocations.
///
/// Userspace has no `malloc` syscall, so a program that wants a heap must carve
/// its own pool out of anonymous memory and hand it to the allocator. The
/// kernel maps anonymous memory eagerly, hence the caller picks `size` to match
/// its real working set instead of over-reserving. Any allocation attempted
/// before this returns `true` hits the empty heap and trips the default
/// alloc-error handler (a panic).
///
/// One-shot: the second successful-or-not call after initialization returns
/// `false`. A failed `mmap` leaves the guard unclaimed so a smaller retry is
/// still possible.
pub fn heap_init(size: usize) -> bool {
    if HEAP_INITIALIZED.load(Ordering::Acquire) {
        return false;
    }

    let addr = mmap(
        0,
        size,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::ANONYMOUS | MapFlags::PRIVATE,
        0,
        0,
    );
    if addr <= 0 {
        return false;
    }

    // Claim the guard only after a successful mmap so a mmap failure above can
    // be retried. Losing the race means another caller already initialized.
    if HEAP_INITIALIZED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }

    // SAFETY: mmap just returned this region exclusively to us. It stays mapped
    // for the whole process lifetime because there is no munmap syscall, so the
    // heap's `'static` requirement holds. The guard above makes init run once.
    unsafe { ALLOCATOR.lock().init(addr as *mut u8, size) };
    true
}
