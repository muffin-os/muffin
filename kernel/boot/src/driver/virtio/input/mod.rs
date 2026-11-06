//! The virtio-input PCI driver. Each device serves one `/dev/input/eventN`,
//! numbered in probe order.

mod devfile;

use alloc::boxed::Box;
use alloc::format;
use alloc::sync::Arc;
use core::error::Error;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::Relaxed;

use kernel_pci::PciAddress;
use kernel_pci::config::ConfigurationAccess;
use kernel_vfs::path::AbsolutePath;
use linkme::distributed_slice;
use tracing::info;
use virtio_drivers::device::input::VirtIOInput;
use virtio_drivers::transport::pci::PciTransport;

use self::devfile::InputHub;
use crate::driver::pci::{PCI_DRIVERS, PciDriverDescriptor, PciDriverType};
use crate::driver::virtio::hal::{HalImpl, transport};
use crate::file::devfs::devfs;

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
    let transport = transport(addr, cam);
    let mut device = InputDevice::new(transport)?;
    let name = device.name()?;

    let path = format!("/input/event{}", NEXT_EVENT_NODE.fetch_add(1, Relaxed));
    info!(%name, %path, "input device");

    let hub = Arc::new(InputHub::new(device));
    devfs()
        .write()
        .register_file(AbsolutePath::try_new(&path)?, move || Ok(hub.open()))?;
    Ok(())
}
