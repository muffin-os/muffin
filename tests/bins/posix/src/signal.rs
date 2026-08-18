use minilib::{
    EFAULT, EINVAL, ESRCH, SYS_KILL, SYS_SIGACTION, SYS_SIGPENDING, SYS_SIGPROCMASK, SaFlags,
    SigAction, SigHandler, SigMaskHow, SigSet, Signal, getpid, kill, ret, sigaction, sigpending,
    sigprocmask, sigreturn_restorer, syscall1, syscall2, syscall3,
};

use crate::check;

const KERNEL_PTR: usize = 0xFFFF_8000_0000_0000;
const BAD_SIGNO: usize = 99;
const BAD_HOW: usize = 99;
const MISSING_PID: usize = 999_999;

const TOUCHED: [Signal; 5] = [
    Signal::Usr1,
    Signal::Usr2,
    Signal::Urgent,
    Signal::Kill,
    Signal::Stop,
];

extern "C" fn usr1_handler(_signo: Signal) {}

fn query(name: &str, signo: Signal) -> SigAction {
    let mut action = SigAction::default();
    check::expect_ok(name, sigaction(signo, None, Some(&mut action)), ());
    action
}

fn mask(name: &str) -> SigSet {
    let mut set: SigSet = 0;
    check::expect_ok(
        name,
        sigprocmask(SigMaskHow::Block, None, Some(&mut set)),
        (),
    );
    set
}

pub fn run() {
    check::group("signal");

    let pid = getpid();

    let baseline_mask = mask("signal/mask_query");
    let baseline: [SigAction; TOUCHED.len()] =
        core::array::from_fn(|i| query("signal/sigaction_query", TOUCHED[i]));

    check::require(
        "signal/sigaction_default_disposition",
        baseline[0] == SigAction::default(),
    );

    check::expect_ok(
        "signal/kill_sig0_self",
        ret(syscall2(SYS_KILL, pid as usize, 0)),
        0,
    );
    check::expect_err(
        "signal/kill_sig0_missing_pid",
        ret(syscall2(SYS_KILL, MISSING_PID, 0)),
        ESRCH,
    );
    check::expect_err(
        "signal/kill_missing_pid",
        kill(MISSING_PID as i64, Signal::Terminate),
        ESRCH,
    );
    check::expect_err(
        "signal/kill_bad_signo",
        ret(syscall2(SYS_KILL, pid as usize, BAD_SIGNO)),
        EINVAL,
    );

    let installed = SigAction {
        handler: SigHandler::new(usr1_handler as *const () as usize),
        mask: 0,
        flags: SaFlags::default(),
        restorer: sigreturn_restorer as *const () as usize,
    };
    check::expect_ok(
        "signal/sigaction_install",
        sigaction(Signal::Usr1, Some(&installed), None),
        (),
    );
    check::require(
        "signal/sigaction_roundtrip",
        query("signal/sigaction_query_installed", Signal::Usr1) == installed,
    );
    check::expect_ok(
        "signal/sigaction_uninstall",
        sigaction(Signal::Usr1, Some(&baseline[0]), None),
        (),
    );

    check::expect_err(
        "signal/sigaction_kill_protected",
        sigaction(Signal::Kill, Some(&installed), None),
        EINVAL,
    );
    check::expect_err(
        "signal/sigaction_stop_protected",
        sigaction(Signal::Stop, Some(&installed), None),
        EINVAL,
    );

    let no_restorer = SigAction {
        restorer: 0,
        ..installed
    };
    check::expect_err(
        "signal/sigaction_needs_restorer",
        sigaction(Signal::Usr2, Some(&no_restorer), None),
        EINVAL,
    );
    let siginfo = SigAction {
        flags: SaFlags::SIGINFO,
        ..installed
    };
    check::expect_err(
        "signal/sigaction_siginfo_unsupported",
        sigaction(Signal::Usr2, Some(&siginfo), None),
        EINVAL,
    );

    check::expect_ok(
        "signal/sigaction_null_new_and_old",
        ret(syscall3(
            SYS_SIGACTION,
            Signal::Usr1.number() as usize,
            0,
            0,
        )),
        0,
    );
    check::expect_err(
        "signal/sigaction_bad_signo",
        ret(syscall3(SYS_SIGACTION, BAD_SIGNO, 0, 0)),
        EINVAL,
    );
    check::expect_err(
        "signal/sigaction_new_efault",
        ret(syscall3(
            SYS_SIGACTION,
            Signal::Usr2.number() as usize,
            KERNEL_PTR,
            0,
        )),
        EFAULT,
    );

    let usr1_bit = Signal::Usr1.bit();
    check::expect_ok(
        "signal/mask_block",
        sigprocmask(SigMaskHow::Block, Some(&usr1_bit), None),
        (),
    );
    check::require(
        "signal/mask_block_effective",
        mask("signal/mask_block_readback") == baseline_mask | usr1_bit,
    );
    check::expect_ok(
        "signal/mask_unblock",
        sigprocmask(SigMaskHow::Unblock, Some(&usr1_bit), None),
        (),
    );
    check::require(
        "signal/mask_unblock_effective",
        mask("signal/mask_unblock_readback") == baseline_mask & !usr1_bit,
    );

    let all: SigSet = SigSet::MAX;
    check::expect_ok(
        "signal/mask_setmask",
        sigprocmask(SigMaskHow::SetMask, Some(&all), None),
        (),
    );
    check::require(
        "signal/mask_kill_stop_unblockable",
        mask("signal/mask_setmask_readback") == all & !(Signal::Kill.bit() | Signal::Stop.bit()),
    );
    check::expect_ok(
        "signal/mask_restore",
        sigprocmask(SigMaskHow::SetMask, Some(&baseline_mask), None),
        (),
    );
    check::expect_err(
        "signal/mask_bad_how",
        ret(syscall3(SYS_SIGPROCMASK, BAD_HOW, 0, 0)),
        EINVAL,
    );

    let mut pending: SigSet = 0;
    check::expect_ok("signal/pending_query", sigpending(&mut pending), ());
    check::require("signal/pending_empty", pending == 0);
    check::expect_err(
        "signal/pending_null",
        ret(syscall1(SYS_SIGPENDING, 0)),
        EINVAL,
    );

    // SIGURG's default disposition is Ignore, so the pending bit this leaves
    // behind can neither terminate the process nor interrupt a later wait once
    // the mask is restored. A signal whose default action terminates would kill
    // the suite at the next timer tick.
    let urgent_bit = Signal::Urgent.bit();
    check::expect_ok(
        "signal/mask_block_urgent",
        sigprocmask(SigMaskHow::Block, Some(&urgent_bit), None),
        (),
    );
    check::expect_ok("signal/kill_self_blocked", kill(pid, Signal::Urgent), ());
    check::expect_ok(
        "signal/pending_after_kill_query",
        sigpending(&mut pending),
        (),
    );
    check::require("signal/pending_after_kill", pending == urgent_bit);
    check::expect_ok(
        "signal/mask_restore_urgent",
        sigprocmask(SigMaskHow::SetMask, Some(&baseline_mask), None),
        (),
    );

    check::require(
        "signal/state_restored",
        mask("signal/state_restored") == baseline_mask,
    );
    for (signo, before) in TOUCHED.iter().zip(baseline.iter()) {
        check::require(
            "signal/state_restored",
            query("signal/state_restored", *signo) == *before,
        );
    }
}
