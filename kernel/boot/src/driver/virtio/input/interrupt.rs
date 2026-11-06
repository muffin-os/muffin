//! Routes the legacy PCI INTx lines of the input devices to one IO APIC
//! vector. The firmware writes the line into the PCI configuration space. On
//! the supported QEMU PC machine, that line is the GSI of the IO APIC.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

use kernel_pci::PciAddress;
use kernel_pci::config::ConfigKey;
use spin::Mutex;
use tracing::info;
use x2apic::ioapic::{IrqFlags, IrqMode, RedirectionTableEntry};
use x86_64::instructions::interrupts;

use super::devfile::InputHub;
use crate::apic::io_apic;
use crate::arch::idt::InterruptIndex;
use crate::driver::pci::VirtIoCam;
use crate::mcore::context::ExecutionContext;
use crate::sync::IrqMutex;

/// The interrupt handler reads this list. A hub stays in the list for the
/// device lifetime, so the handler never drops the last reference.
static HUBS: IrqMutex<Vec<Arc<InputHub>>> = IrqMutex::new(Vec::new());

/// These devices wait for [`enable_input_interrupts`].
static PENDING: Mutex<Vec<PendingRoute>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, Copy)]
pub struct Route {
    index: u8,
    destination: u8,
}

struct PendingRoute {
    addr: PciAddress,
    cam: VirtIoCam,
    route: Route,
}

#[derive(Debug)]
pub enum RouteError {
    NoUsableRoute,
    DestinationTooLarge,
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoUsableRoute => "virtio input has no usable PCI INTx route",
            Self::DestinationTooLarge => "virtio input INTx destination exceeds 255",
        })
    }
}

impl Error for RouteError {}

/// Reads the INTx route of the device. ISA interrupt source overrides do not
/// apply to a PCI line.
pub fn read_route(addr: PciAddress, cam: &VirtIoCam) -> Result<Route, RouteError> {
    let pin = cam.read_u8(addr, ConfigKey::<u8>::INTERRUPT_PIN);
    let line = cam.read_u8(addr, ConfigKey::<u8>::INTERRUPT_LINE);
    if !(1..=4).contains(&pin) || line == 0 || line == 0xff {
        return Err(RouteError::NoUsableRoute);
    }

    let index = {
        let mut io_apic = io_apic().lock();
        let base = io_apic.global_system_interrupt_base();
        // Safety: a read of the version register has no side effects.
        let entries = u32::from(unsafe { io_apic.max_table_entry() }) + 1;
        u32::from(line)
            .checked_sub(base)
            .filter(|index| *index < entries)
            .and_then(|index| u8::try_from(index).ok())
            .ok_or(RouteError::NoUsableRoute)?
    };

    // Safety: interrupts are off, so the context is this CPU's.
    let lapic_id =
        interrupts::without_interrupts(|| unsafe { ExecutionContext::load() }.lapic_id());
    let destination = u8::try_from(lapic_id).map_err(|_| RouteError::DestinationTooLarge)?;

    info!(
        ?addr,
        pin, line, index, destination, "virtio input INTx route"
    );
    Ok(Route { index, destination })
}

/// Adds the hub to the interrupt registry. The route stays masked and the
/// INTx of the device stays disabled until [`enable_input_interrupts`].
pub fn register(hub: Arc<InputHub>, addr: PciAddress, cam: VirtIoCam, route: Route) {
    HUBS.lock_without_interrupts().push(hub);
    PENDING.lock().push(PendingRoute { addr, cam, route });
}

/// Programs each input route, unmasks it, and then enables the INTx of each
/// registered input device.
pub fn enable_input_interrupts() {
    let pending = core::mem::take(&mut *PENDING.lock());

    {
        let mut io_apic = io_apic().lock();
        for (i, entry) in pending.iter().enumerate() {
            // Devices on a shared line use one redirection entry.
            if pending[..i]
                .iter()
                .any(|other| other.route.index == entry.route.index)
            {
                continue;
            }
            let mut table_entry = RedirectionTableEntry::default();
            table_entry.set_mode(IrqMode::Fixed);
            table_entry
                .set_flags(IrqFlags::LOW_ACTIVE | IrqFlags::LEVEL_TRIGGERED | IrqFlags::MASKED);
            table_entry.set_vector(InterruptIndex::VirtioInput.as_u8());
            table_entry.set_dest(entry.route.destination);
            // Safety: `read_route` checked that the index is inside the table.
            unsafe { io_apic.set_table_entry(entry.route.index, table_entry) };
        }
        for entry in &pending {
            // Safety: the entry above is complete.
            unsafe { io_apic.enable_irq(entry.route.index) };
        }
    }

    for entry in &pending {
        entry.cam.set_intx_enabled(entry.addr, true);
    }
}

/// Acknowledges each registered device. This runs in the interrupt handler,
/// so it must not allocate or take reader locks.
pub fn handle_input_interrupt() {
    let mut i = 0;
    loop {
        let Some(hub) = HUBS.lock_without_interrupts().get(i).cloned() else {
            return;
        };
        hub.acknowledge();
        i += 1;
    }
}
