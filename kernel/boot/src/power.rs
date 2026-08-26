use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Release};
use core::sync::atomic::{AtomicBool, AtomicUsize};
use core::time::Duration;

use acpi::address::AddressSpace;
use acpi::fadt::Fadt;
use conquer_once::spin::OnceCell;
use kernel_abi::Signal;
use kernel_poweroff::{Pm1Ports, PowerOffIgnored, power_off};
use kernel_syscall::signal::{SignalTarget, sys_kill};
use tracing::{Level, debug, error, info, instrument, warn};
use x2apic::lapic::IpiAllShorthand;
use x86_64::instructions::{hlt, interrupts};

use crate::arch::idt::InterruptIndex;
use crate::hpet::hpet;
use crate::limine::MP_REQUEST;
use crate::mcore::context::ExecutionContext;
use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::process::tree::process_tree;
use crate::syscall::access::KernelAccess;

static PM1_PORTS: OnceCell<Pm1Ports> = OnceCell::uninit();
static HALTED_CPUS: AtomicUsize = AtomicUsize::new(0);
static SHUTDOWN_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Bounded grace for SIGTERMed processes to exit.
const TERM_GRACE: Duration = Duration::from_secs(2);
/// Bounded wait for halt-IPI acknowledgements.
const HALT_ACK_GRACE: Duration = Duration::from_millis(100);

#[instrument(name = "init power interface", level = Level::DEBUG)]
pub fn init() {
    let tables = crate::acpi::acpi_tables().lock();
    let fadt = match tables.find_table::<Fadt>() {
        Ok(fadt) => fadt,
        Err(e) => {
            warn!(error = ?e, "no FADT, ACPI power-off unavailable");
            return;
        }
    };
    let pm1a = match fadt.pm1a_control_block() {
        Ok(block) if block.address_space == AddressSpace::SystemIo => block.address as u16,
        Ok(block) => {
            warn!(
                address_space = ?block.address_space,
                "PM1a control block is not port I/O, ACPI power-off unavailable"
            );
            return;
        }
        Err(e) => {
            warn!(error = ?e, "no PM1a control block, ACPI power-off unavailable");
            return;
        }
    };
    let pm1b = fadt
        .pm1b_control_block()
        .ok()
        .flatten()
        .filter(|block| block.address_space == AddressSpace::SystemIo)
        .map(|block| block.address as u16);

    PM1_PORTS.init_once(|| Pm1Ports { a: pm1a, b: pm1b });
}

pub(crate) fn note_cpu_halted() {
    HALTED_CPUS.fetch_add(1, Release);
}

/// Powers the machine off. SIGTERMs every process, waits bounded for exits,
/// halts the other cores, then writes SLP_EN to the PM1 control ports.
///
/// Runs in the caller's syscall context with interrupts enabled, so the
/// timer keeps preempting this task while dying processes run.
pub fn shutdown() -> ! {
    if SHUTDOWN_IN_PROGRESS
        .compare_exchange(false, true, AcqRel, Acquire)
        .is_err()
    {
        debug!("shutdown already in progress");
        loop {
            hlt();
        }
    }

    info!("shutdown requested");

    let _ = sys_kill(
        &KernelAccess::new(),
        SignalTarget::BroadcastAll,
        Signal::Terminate,
    );

    let root_pid = Process::root().pid();
    let caller_pid = Process::current().pid();
    let victims: Vec<Arc<Process>> = process_tree()
        .read()
        .all()
        .filter(|p| p.pid() != root_pid && p.pid() != caller_pid)
        .cloned()
        .collect();

    let deadline = hpet().read().elapsed_ns() + TERM_GRACE.as_nanos() as u64;
    while !victims.iter().all(|p| p.exit_outcome().is_some()) {
        if hpet().read().elapsed_ns() >= deadline {
            let survivors = victims
                .iter()
                .filter(|p| p.exit_outcome().is_none())
                .count();
            warn!(survivors, "termination grace expired, powering off anyway");
            break;
        }
        hlt();
    }

    info!("powering off");

    let cpu_count = unsafe {
        #[allow(static_mut_refs)] // only written during boot, read-only here
        MP_REQUEST.get_response()
    }
    .expect("MP response should exist")
    .cpus()
    .len();

    interrupts::disable();

    unsafe {
        // Safety: interrupts were disabled above, so the context is this
        // CPU's and the IPI goes out through the local APIC.
        ExecutionContext::load().lapic().lock().send_ipi_all(
            InterruptIndex::Halt.as_u8(),
            IpiAllShorthand::AllExcludingSelf,
        );
    }

    let ack_deadline = hpet().read().elapsed_ns() + HALT_ACK_GRACE.as_nanos() as u64;
    while HALTED_CPUS.load(Acquire) < cpu_count - 1 && hpet().read().elapsed_ns() < ack_deadline {
        core::hint::spin_loop();
    }

    if let Some(&ports) = PM1_PORTS.get() {
        let Err(PowerOffIgnored) = power_off(ports);
    }

    error!("ACPI power-off had no effect, halting");
    loop {
        hlt();
    }
}
