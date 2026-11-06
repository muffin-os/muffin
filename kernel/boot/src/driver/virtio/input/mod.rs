//! The virtio-input PCI driver. Each device serves one `/dev/input/eventN`,
//! numbered in probe order.

mod devfile;
mod interrupt;

use alloc::boxed::Box;
use alloc::format;
use alloc::sync::Arc;
use core::error::Error;
use core::ffi::c_void;
use core::sync::atomic::Ordering::Relaxed;
use core::sync::atomic::{AtomicUsize, Ordering};

use kernel_pci::PciAddress;
use kernel_pci::config::ConfigurationAccess;
use kernel_vfs::path::AbsolutePath;
use linkme::distributed_slice;
use tracing::info;
use virtio_drivers::device::input::VirtIOInput;
use virtio_drivers::transport::pci::PciTransport;

use self::devfile::InputHub;
pub use self::interrupt::{enable_input_interrupts, handle_input_interrupt};
use crate::driver::pci::{PCI_DRIVERS, PciDriverDescriptor, PciDriverType, VirtIoCam};
use crate::driver::virtio::hal::{HalImpl, transport};
use crate::file::devfs::devfs;
use crate::mcore::mtask::process::{ParkOutcome, Process};
use crate::mcore::mtask::scheduler::global::GlobalTaskQueue;
use crate::mcore::mtask::task::Task;

pub type InputDevice = VirtIOInput<HalImpl, PciTransport>;

#[distributed_slice(PCI_DRIVERS)]
static VIRTIO_INPUT: PciDriverDescriptor = PciDriverDescriptor {
    name: "virtio-input",
    typ: PciDriverType::Specific,
    probe: virtio_probe,
    init: virtio_init,
};

fn virtio_probe(addr: PciAddress, cam: &dyn ConfigurationAccess) -> bool {
    addr.vendor_id(cam) == 0x1af4 && addr.device_id(cam) == 0x1052
}

static NEXT_EVENT_NODE: AtomicUsize = AtomicUsize::new(0);

fn virtio_init(addr: PciAddress, cam: Box<dyn ConfigurationAccess>) -> Result<(), Box<dyn Error>> {
    let cam = VirtIoCam::new(cam);
    cam.set_intx_enabled(addr, false);
    let route = interrupt::read_route(addr, &cam)?;
    let transport = transport(addr, &cam);
    let mut device = InputDevice::new(transport)?;
    let name = device.name()?;

    let path = format!("/input/event{}", NEXT_EVENT_NODE.fetch_add(1, Relaxed));
    info!(%name, %path, "input device");

    let hub = Arc::new(InputHub::new(device));
    let node_hub = hub.clone();
    devfs()
        .write()
        .register_file(AbsolutePath::try_new(&path)?, move || Ok(node_hub.open()))?;
    interrupt::register(hub.clone(), addr, cam, route);
    start_worker(hub);
    Ok(())
}

fn start_worker(hub: Arc<InputHub>) {
    let arg = Arc::into_raw(hub).cast_mut().cast::<c_void>();
    let task = Task::create_new(Process::root(), input_worker, arg)
        .expect("should be able to create the input worker task");
    GlobalTaskQueue::enqueue(Box::pin(task));
}

/// Moves device events into the reader queues. The interrupt handler sets
/// `pending`, and the worker exists for the device lifetime.
extern "C" fn input_worker(arg: *mut c_void) {
    // Safety: `start_worker` gives this task one reference from `Arc::into_raw`.
    let hub = unsafe { Arc::from_raw(arg.cast_const().cast::<InputHub>()) };
    let process = Process::current();
    loop {
        while hub.pending.swap(false, Ordering::AcqRel) {
            hub.drain();
        }
        let outcome = process.park_current_task(None, &[&hub.work_ready], || {
            hub.pending.load(Ordering::Acquire)
        });
        if matches!(outcome, ParkOutcome::Interrupted) {
            Task::exit_current();
        }
    }
}
