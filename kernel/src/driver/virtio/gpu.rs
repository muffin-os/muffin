use alloc::boxed::Box;
use alloc::sync::Arc;
use core::error::Error;
use core::fmt::{Debug, Formatter};
use core::ptr::NonNull;
use core::slice;

use kernel_devfs::DevFile;
use kernel_device::Device;
use kernel_device::raw::RawDevice;
use kernel_pci::PciAddress;
use kernel_pci::config::ConfigurationAccess;
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{FsyncError, MmapError, MmapRegion, ReadError, Stat, StatError, WriteError};
use linkme::distributed_slice;
use spin::Mutex;
use spin::rwlock::RwLock;
use virtio_drivers::device::gpu::VirtIOGpu;
use virtio_drivers::transport::pci::PciTransport;
use x86_64::VirtAddr;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PhysFrame, Size4KiB};

use crate::UsizeExt;
use crate::driver::KernelDeviceId;
use crate::driver::pci::{PCI_DRIVERS, PciDriverDescriptor, PciDriverType};
use crate::driver::raw::RawDevices;
use crate::driver::virtio::hal::{HalImpl, transport};
use crate::file::devfs::devfs;
use crate::mem::address_space::AddressSpace;

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

#[allow(clippy::needless_pass_by_value)] // signature is required like this
fn virtio_init(addr: PciAddress, cam: Box<dyn ConfigurationAccess>) -> Result<(), Box<dyn Error>> {
    let transport = transport(addr, cam);

    let mut gpu = VirtIOGpu::<HalImpl, _>::new(transport)?;
    let (width, height) = gpu.resolution()?;
    let logical_len = width as usize * height as usize * 4;

    let fb = gpu.setup_framebuffer()?;
    let buffer_virtual_addr = VirtAddr::from_ptr(fb);
    let buffer_len = fb.len();
    let fb_ptr = NonNull::new(fb.as_mut_ptr()).expect("framebuffer pointer should be non-null");

    let phys_addr = AddressSpace::kernel()
        .translate(buffer_virtual_addr)
        .expect("address should be mapped into kernel space");
    let start = PhysFrame::<Size4KiB>::containing_address(phys_addr);
    let end = PhysFrame::<Size4KiB>::containing_address(phys_addr + buffer_len.into_u64() - 1);
    let physical_memory = PhysFrameRangeInclusive { start, end };

    let gpu_arc = Arc::new(Mutex::new(gpu));

    {
        let gpu_arc = gpu_arc.clone();
        let fb_addr = fb_ptr.as_ptr() as usize;
        devfs()
            .write()
            .register_file(AbsolutePath::try_new("/fb0").unwrap(), move || {
                Ok(FbDevFile {
                    gpu: gpu_arc.clone(),
                    ptr: NonNull::new(fb_addr as *mut u8).unwrap(),
                    len: logical_len,
                })
            })
            .expect("should be able to register /dev/fb0");
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

/// Device file backing `/dev/fb0`. Holds a shared handle to the underlying
/// GPU so `fsync` can flush it, and a raw pointer to the HHDM-mapped
/// framebuffer bytes so `mmap` can hand them out.
struct FbDevFile {
    gpu: Arc<Mutex<VirtIOGpu<HalImpl, PciTransport>>>,
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: `ptr` aliases a framebuffer that lives for the kernel's lifetime;
// access is gated behind `&mut self` on this `DevFile`, which `DevFs` already
// serializes via its open-file map.
unsafe impl Send for FbDevFile {}
unsafe impl Sync for FbDevFile {}

impl FbDevFile {
    fn as_slice_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl DevFile for FbDevFile {
    fn read(&mut self, buf: &mut [u8], offset: usize) -> Result<usize, ReadError> {
        if offset >= self.len {
            return Err(ReadError::EndOfFile);
        }
        let n = buf.len().min(self.len - offset);
        let fb = self.as_slice_mut();
        buf[..n].copy_from_slice(&fb[offset..offset + n]);
        Ok(n)
    }

    fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        if offset >= self.len {
            return Err(WriteError::WriteFailed);
        }
        let n = buf.len().min(self.len - offset);
        let fb = self.as_slice_mut();
        fb[offset..offset + n].copy_from_slice(&buf[..n]);
        Ok(n)
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = self.len;
        Ok(())
    }

    fn mmap(&mut self) -> Result<MmapRegion, MmapError> {
        Ok(MmapRegion {
            ptr: self.ptr,
            len: self.len,
        })
    }

    fn fsync(&mut self) -> Result<(), FsyncError> {
        self.gpu.lock().flush().map_err(|_| FsyncError::Failed)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct VirtIoRawDevice {
    id: KernelDeviceId,
    _inner: Arc<Mutex<VirtIOGpu<HalImpl, PciTransport>>>,
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
