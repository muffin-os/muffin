//! The virtio-gpu PCI driver. It serves `/dev/fb0` (2D framebuffer) and
//! `/dev/gpu0` (virgl command submission).

mod devfile;
mod device;
mod wire;

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::error::Error;
use core::fmt::{Debug, Formatter};

use kernel_device::Device;
use kernel_device::raw::RawDevice;
use kernel_pci::PciAddress;
use kernel_pci::config::ConfigurationAccess;
use kernel_vfs::OpenError;
use kernel_vfs::path::AbsolutePath;
use linkme::distributed_slice;
use spin::Mutex;
use spin::rwlock::RwLock;
use x86_64::PhysAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PhysFrame, Size4KiB};

use self::devfile::{FbDevFile, FbMemory, GpuDevFile};
use self::device::GpuDevice;
use crate::UsizeExt;
use crate::driver::KernelDeviceId;
use crate::driver::pci::{PCI_DRIVERS, PciDriverDescriptor, PciDriverType};
use crate::driver::raw::RawDevices;
use crate::driver::virtio::hal::transport;
use crate::file::devfs::devfs;

#[distributed_slice(PCI_DRIVERS)]
static VIRTIO_GPU: PciDriverDescriptor = PciDriverDescriptor {
    name: "virtio-gpu",
    typ: PciDriverType::Specific,
    probe: virtio_probe,
    init: virtio_init,
};

fn virtio_probe(addr: PciAddress, cam: &dyn ConfigurationAccess) -> bool {
    addr.vendor_id(cam) == 0x1af4 && addr.device_id(cam) == 0x1050
}

fn virtio_init(addr: PciAddress, cam: Box<dyn ConfigurationAccess>) -> Result<(), Box<dyn Error>> {
    let transport = transport(addr, cam);
    let mut gpu = GpuDevice::new(transport)?;
    let scanout = gpu.scanout();
    let (width, height) = (scanout.width, scanout.height);
    let logical_len = width as usize * height as usize * 4;

    let framebuffer = gpu.create_framebuffer()?;
    let fb_addr = framebuffer.ptr().as_ptr() as usize;
    let phys_addr = PhysAddr::new(framebuffer.paddr());
    let start = PhysFrame::<Size4KiB>::containing_address(phys_addr);
    let end = PhysFrame::<Size4KiB>::containing_address(phys_addr + logical_len.into_u64() - 1);
    let physical_memory = PhysFrameRangeInclusive { start, end };

    let gpu_arc = Arc::new(Mutex::new(gpu));

    {
        let gpu_arc = gpu_arc.clone();
        devfs()
            .write()
            .register_file(AbsolutePath::try_new("/fb0").unwrap(), move || {
                Ok(FbDevFile {
                    device: gpu_arc.clone(),
                    memory: Arc::new(FbMemory {
                        ptr: core::ptr::NonNull::new(fb_addr as *mut u8).unwrap(),
                        len: logical_len,
                    }),
                    width,
                    height,
                })
            })
            .expect("should be able to register /dev/fb0");
    }

    {
        let gpu_arc = gpu_arc.clone();
        devfs()
            .write()
            .register_file(AbsolutePath::try_new("/gpu0").unwrap(), move || {
                GpuDevFile::open(gpu_arc.clone()).map_err(|_| OpenError::OpenFailed)
            })
            .expect("should be able to register /dev/gpu0");
    }

    let device = VirtIoRawDevice {
        id: KernelDeviceId::new(),
        _inner: gpu_arc,
        physical_memory,
    };
    let device = Arc::new(RwLock::new(device));

    RawDevices::register_raw_device(device)?;

    Ok(())
}

#[derive(Clone)]
pub struct VirtIoRawDevice {
    id: KernelDeviceId,
    _inner: Arc<Mutex<GpuDevice>>,
    physical_memory: PhysFrameRangeInclusive,
}

impl Debug for VirtIoRawDevice {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VirtIoRawDevice")
            .field("id", &self.id)
            .field("physical_memory", &self.physical_memory)
            .finish_non_exhaustive()
    }
}

impl Device<KernelDeviceId> for VirtIoRawDevice {
    fn id(&self) -> KernelDeviceId {
        self.id
    }
}

impl RawDevice<KernelDeviceId> for VirtIoRawDevice {
    fn physical_memory(&self) -> PhysFrameRangeInclusive {
        self.physical_memory
    }
}
