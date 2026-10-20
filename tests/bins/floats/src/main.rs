#![no_std]
#![no_main]

//! Userspace exerciser for the kernel's lazy x87 and SSE save and restore.
//!
//! Every expected value is derived from `getpid`, so two concurrent instances
//! never share register contents. That is what makes a save writing the wrong
//! task's state detectable at all. The host test documents the two
//! preconditions that keep this able to fail.

use core::arch::asm;

use minilib::{SYS_KILL, Signal, getpid, install_handler, println};

/// Iterations per spin. A tight `dec` and `jnz` loop, so it retires much faster
/// than iteration counts elsewhere in the test suite suggest. It has to stay
/// well above one timer tick. Below that no preemption lands inside the window
/// while the values under test are live in registers, and every check passes
/// without exercising the kernel at all.
const SPIN: u64 = 20_000_000;

fn expect(ok: bool, pass: &str, fail: &str) -> bool {
    println!("{}", if ok { pass } else { fail });
    ok
}

/// Sixteen distinct finite doubles keyed by index and pid. Distinct per index
/// so a register swap is caught, distinct per pid so a cross task leak is.
fn pattern(pid: u64, tag: u64) -> [u64; 16] {
    let mut p = [0u64; 16];
    let mut i = 0u64;
    while i < 16 {
        p[i as usize] = tag | (i << 32) | (pid << 8) | i;
        i += 1;
    }
    p
}

fn check_xmm(pid: u64) -> bool {
    let input = pattern(pid, 0x4010_0000_0000_0000);
    for _ in 0..4 {
        let mut out = [0u64; 16];
        let cnt = SPIN;
        // The load, the spin and the readback share one `asm!` block. As separate
        // Rust code the compiler spills the xmm registers around the spin, so the
        // values would survive on the stack even against a broken kernel save.
        // Every check below that must survive a context switch keeps its spin
        // inside the block for the same reason.
        unsafe {
            asm!(
                "movsd xmm0,  [{inp}]",
                "movsd xmm1,  [{inp} + 8]",
                "movsd xmm2,  [{inp} + 16]",
                "movsd xmm3,  [{inp} + 24]",
                "movsd xmm4,  [{inp} + 32]",
                "movsd xmm5,  [{inp} + 40]",
                "movsd xmm6,  [{inp} + 48]",
                "movsd xmm7,  [{inp} + 56]",
                "movsd xmm8,  [{inp} + 64]",
                "movsd xmm9,  [{inp} + 72]",
                "movsd xmm10, [{inp} + 80]",
                "movsd xmm11, [{inp} + 88]",
                "movsd xmm12, [{inp} + 96]",
                "movsd xmm13, [{inp} + 104]",
                "movsd xmm14, [{inp} + 112]",
                "movsd xmm15, [{inp} + 120]",
                "2:",
                "dec {cnt}",
                "jnz 2b",
                "movsd [{outp}],       xmm0",
                "movsd [{outp} + 8],   xmm1",
                "movsd [{outp} + 16],  xmm2",
                "movsd [{outp} + 24],  xmm3",
                "movsd [{outp} + 32],  xmm4",
                "movsd [{outp} + 40],  xmm5",
                "movsd [{outp} + 48],  xmm6",
                "movsd [{outp} + 56],  xmm7",
                "movsd [{outp} + 64],  xmm8",
                "movsd [{outp} + 72],  xmm9",
                "movsd [{outp} + 80],  xmm10",
                "movsd [{outp} + 88],  xmm11",
                "movsd [{outp} + 96],  xmm12",
                "movsd [{outp} + 104], xmm13",
                "movsd [{outp} + 112], xmm14",
                "movsd [{outp} + 120], xmm15",
                inp = in(reg) input.as_ptr(),
                outp = in(reg) out.as_mut_ptr(),
                cnt = inout(reg) cnt => _,
                out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
                out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                options(nostack),
            );
        }
        let mut i = 0;
        while i < 16 {
            if out[i] != input[i] {
                return false;
            }
            i += 1;
        }
    }
    true
}

/// `fstp` pops the value `fld` pushed, so the x87 stack is balanced on exit
/// and needs no clobber declaration. A double survives the trip through the
/// 80-bit x87 stack unchanged, so the readback is compared raw.
fn check_x87(pid: u64) -> bool {
    let input: u64 = 0x4000_0000_0000_0000 | (pid << 32) | pid;
    let mut out: u64 = 0;
    let cnt = SPIN;
    unsafe {
        asm!(
            "fld qword ptr [{inp}]",
            "2:",
            "dec {cnt}",
            "jnz 2b",
            "fstp qword ptr [{outp}]",
            inp = in(reg) &input,
            outp = in(reg) &mut out,
            cnt = inout(reg) cnt => _,
            options(nostack),
        );
    }
    out == input
}

/// Round toward zero and flush to zero stay in effect process wide, so the
/// original `MXCSR` has to be put back or every later float in this process
/// silently changes behaviour. The sticky exception flags carry the pid; they
/// are masked, so setting them traps nothing.
fn check_mxcsr(pid: u64) -> bool {
    const ROUND_ZERO_FLUSH: u32 = 0xE000;
    const EXCEPTION_FLAGS: u32 = 0x3F;
    let mut orig: u32 = 0;
    unsafe {
        asm!("stmxcsr [{p}]", p = in(reg) &mut orig, options(nostack));
    }
    let modified = (orig & !EXCEPTION_FLAGS) | ROUND_ZERO_FLUSH | (pid as u32 & EXCEPTION_FLAGS);
    let mut readback: u32 = 0;
    let cnt = SPIN;
    unsafe {
        asm!(
            "ldmxcsr [{m}]",
            "2:",
            "dec {cnt}",
            "jnz 2b",
            "stmxcsr [{r}]",
            "ldmxcsr [{o}]",
            m = in(reg) &modified,
            r = in(reg) &mut readback,
            o = in(reg) &orig,
            cnt = inout(reg) cnt => _,
            options(nostack),
        );
    }
    readback == modified
}

/// Proves the rounding mode is semantically live, not merely byte preserved.
/// `cvtsd2si` honours `MXCSR`, so `pid + 0.75` truncates to `pid` under round
/// toward zero and rounds to `pid + 1` under round to nearest. Requiring both
/// answers fails a mode stuck in either state.
fn check_rounding(pid: u64) -> bool {
    let mut orig: u32 = 0;
    unsafe {
        asm!("stmxcsr [{p}]", p = in(reg) &mut orig, options(nostack));
    }
    let rz = orig | 0x6000;
    let rn = orig & !0x6000u32;
    let val: u64 = (pid as f64 + 0.75).to_bits();
    let mut r_zero: i64 = 0;
    let mut r_near: i64 = 0;
    let cnt = SPIN;
    unsafe {
        asm!(
            "movsd xmm0, [{v}]",
            "ldmxcsr [{rz}]",
            "2:",
            "dec {cnt}",
            "jnz 2b",
            "cvtsd2si {a}, xmm0",
            "ldmxcsr [{rn}]",
            "cvtsd2si {b}, xmm0",
            "ldmxcsr [{o}]",
            v = in(reg) &val,
            rz = in(reg) &rz,
            rn = in(reg) &rn,
            o = in(reg) &orig,
            a = out(reg) r_zero,
            b = out(reg) r_near,
            cnt = inout(reg) cnt => _,
            out("xmm0") _,
            options(nostack),
        );
    }
    r_zero == pid as i64 && r_near == pid as i64 + 1
}

/// NaN is never equal to itself, so a float comparison here would be
/// meaningless. The round trip is checked on the raw `u64` bit patterns. A save
/// path that canonicalises the NaN payload or normalises the subnormal fails
/// here.
fn check_special(pid: u64) -> bool {
    let nan_in: u64 = 0x7FF8_0000_DEAD_0000 | pid;
    let sub_in: u64 = pid;
    let mut nan_out: u64 = 0;
    let mut sub_out: u64 = 0;
    let cnt = SPIN;
    unsafe {
        asm!(
            "movsd xmm0, [{ni}]",
            "movsd xmm1, [{si}]",
            "2:",
            "dec {cnt}",
            "jnz 2b",
            "movsd [{no}], xmm0",
            "movsd [{so}], xmm1",
            ni = in(reg) &nan_in,
            si = in(reg) &sub_in,
            no = in(reg) &mut nan_out,
            so = in(reg) &mut sub_out,
            cnt = inout(reg) cnt => _,
            out("xmm0") _, out("xmm1") _,
            options(nostack),
        );
    }
    nan_out == nan_in && sub_out == sub_in
}

/// Every partial sum stays an integer below 2^53, so each `addsd` is exact and
/// the closed form has no rounding slack. The accumulation loop is the spin, so
/// the accumulator is live in xmm0 across every preemption it spans.
fn check_sum(pid: u64) -> bool {
    let one: u64 = 0x3FF0_0000_0000_0000;
    let n: u64 = SPIN + pid;
    let mut result: u64 = 0;
    let cnt = n;
    unsafe {
        asm!(
            "xorps xmm0, xmm0",
            "movsd xmm1, [{one}]",
            "2:",
            "addsd xmm0, xmm1",
            "dec {cnt}",
            "jnz 2b",
            "movsd [{res}], xmm0",
            one = in(reg) &one,
            res = in(reg) &mut result,
            cnt = inout(reg) cnt => _,
            out("xmm0") _, out("xmm1") _,
            options(nostack),
        );
    }
    result == (n as f64).to_bits()
}

extern "C" fn clobber_handler(_signo: Signal) {
    let pat: u64 = 0xBAD0_C0DE_DEAD_BEEF;
    // Overwriting every xmm register is the point. If the signal FPU path fails
    // to restore the interrupted context, this pattern leaks back into the
    // caller and the readback there diverges.
    unsafe {
        asm!(
            "movq xmm0, {p}",
            "movq xmm1, {p}",
            "movq xmm2, {p}",
            "movq xmm3, {p}",
            "movq xmm4, {p}",
            "movq xmm5, {p}",
            "movq xmm6, {p}",
            "movq xmm7, {p}",
            "movq xmm8, {p}",
            "movq xmm9, {p}",
            "movq xmm10, {p}",
            "movq xmm11, {p}",
            "movq xmm12, {p}",
            "movq xmm13, {p}",
            "movq xmm14, {p}",
            "movq xmm15, {p}",
            p = in(reg) pat,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
            options(nostack, nomem),
        );
    }
}

fn check_signal(pid: u64) -> bool {
    let input = pattern(pid, 0x4020_0000_0000_0000);
    let mut out = [0u64; 16];
    let signo = Signal::Usr1.number() as usize;
    // Handlers are delivered at syscall exit, so the `int 0x80` sits between the
    // load and the readback in one block to keep the xmm values live in
    // registers at that exit. The kernel preserves every register except `rax`.
    unsafe {
        asm!(
            "movsd xmm0,  [{inp}]",
            "movsd xmm1,  [{inp} + 8]",
            "movsd xmm2,  [{inp} + 16]",
            "movsd xmm3,  [{inp} + 24]",
            "movsd xmm4,  [{inp} + 32]",
            "movsd xmm5,  [{inp} + 40]",
            "movsd xmm6,  [{inp} + 48]",
            "movsd xmm7,  [{inp} + 56]",
            "movsd xmm8,  [{inp} + 64]",
            "movsd xmm9,  [{inp} + 72]",
            "movsd xmm10, [{inp} + 80]",
            "movsd xmm11, [{inp} + 88]",
            "movsd xmm12, [{inp} + 96]",
            "movsd xmm13, [{inp} + 104]",
            "movsd xmm14, [{inp} + 112]",
            "movsd xmm15, [{inp} + 120]",
            "int 0x80",
            "movsd [{outp}],       xmm0",
            "movsd [{outp} + 8],   xmm1",
            "movsd [{outp} + 16],  xmm2",
            "movsd [{outp} + 24],  xmm3",
            "movsd [{outp} + 32],  xmm4",
            "movsd [{outp} + 40],  xmm5",
            "movsd [{outp} + 48],  xmm6",
            "movsd [{outp} + 56],  xmm7",
            "movsd [{outp} + 64],  xmm8",
            "movsd [{outp} + 72],  xmm9",
            "movsd [{outp} + 80],  xmm10",
            "movsd [{outp} + 88],  xmm11",
            "movsd [{outp} + 96],  xmm12",
            "movsd [{outp} + 104], xmm13",
            "movsd [{outp} + 112], xmm14",
            "movsd [{outp} + 120], xmm15",
            inp = in(reg) input.as_ptr(),
            outp = in(reg) out.as_mut_ptr(),
            inout("rax") SYS_KILL => _,
            in("rdi") 0usize,
            in("rsi") signo,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
            options(nostack),
        );
    }
    let mut i = 0;
    while i < 16 {
        if out[i] != input[i] {
            return false;
        }
        i += 1;
    }
    true
}

minilib::entry!(main);

fn main() -> i32 {
    let _ = install_handler(Signal::Usr1, clobber_handler);

    let pid = getpid() as u64;
    println!("floats: pid={pid} start");

    let ok = expect(check_xmm(pid), "floats: xmm ok", "floats: FAIL xmm")
        && expect(check_x87(pid), "floats: x87 ok", "floats: FAIL x87")
        && expect(check_mxcsr(pid), "floats: mxcsr ok", "floats: FAIL mxcsr")
        && expect(
            check_rounding(pid),
            "floats: rounding ok",
            "floats: FAIL rounding",
        )
        && expect(
            check_special(pid),
            "floats: special ok",
            "floats: FAIL special",
        )
        && expect(check_sum(pid), "floats: sum ok", "floats: FAIL sum")
        && expect(
            check_signal(pid),
            "floats: signal ok",
            "floats: FAIL signal",
        );
    if !ok {
        return 1;
    }

    println!("floats: pid={pid} done");
    0
}
