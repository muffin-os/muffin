use alloc::boxed::Box;
use alloc::sync::Arc;
use core::error::Error;
use core::fmt::{Debug, Formatter};
use core::ptr::NonNull;
use core::slice;

use kernel_abi::{FbScreenInfo, IoctlRequest, S_IFCHR};
use kernel_devfs::DevFile;
use kernel_device::Device;
use kernel_device::raw::RawDevice;
use kernel_pci::PciAddress;
use kernel_pci::config::ConfigurationAccess;
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{
    FsyncError, IoctlError, MappedMemory, MmapError, MmapRegion, ReadError, Stat, StatError,
    WriteError,
};
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
                    memory: Arc::new(FbMemory {
                        ptr: NonNull::new(fb_addr as *mut u8).unwrap(),
                        len: logical_len,
                    }),
                    width,
                    height,
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

/// The framebuffer bytes that `/dev/fb0` lends out for shared mappings.
#[derive(Debug)]
struct FbMemory {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: `ptr` aliases a framebuffer that lives for the kernel's lifetime,
// and every access through it is unsafe on the caller.
unsafe impl Send for FbMemory {}
unsafe impl Sync for FbMemory {}

impl MappedMemory for FbMemory {
    fn ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    fn len(&self) -> usize {
        self.len
    }
}

/// Device file backing `/dev/fb0`. Holds a shared handle to the underlying
/// GPU so `fsync` can flush it, and the framebuffer memory so `mmap` can
/// lend it out.
struct FbDevFile {
    gpu: Arc<Mutex<VirtIOGpu<HalImpl, PciTransport>>>,
    memory: Arc<FbMemory>,
    width: u32,
    height: u32,
}

impl FbDevFile {
    fn as_slice_mut(&mut self) -> &mut [u8] {
        // SAFETY: `DevFs` serializes access through `&mut self`.
        unsafe { slice::from_raw_parts_mut(self.memory.ptr.as_ptr(), self.memory.len) }
    }
}

impl DevFile for FbDevFile {
    fn read(&mut self, buf: &mut [u8], offset: usize) -> Result<usize, ReadError> {
        let fb = self.as_slice_mut();
        let offset = offset.min(fb.len());
        let n = buf.len().min(fb.len() - offset);
        buf[..n].copy_from_slice(&fb[offset..offset + n]);
        Ok(n)
    }

    fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        let fb = self.as_slice_mut();
        if offset >= fb.len() {
            return Err(WriteError::WriteFailed);
        }
        let n = buf.len().min(fb.len() - offset);
        fb[offset..offset + n].copy_from_slice(&buf[..n]);
        Ok(n)
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = self.memory.len;
        stat.mode = S_IFCHR | 0o666;
        Ok(())
    }

    fn mmap(&mut self) -> Result<MmapRegion, MmapError> {
        Ok(self.memory.clone())
    }

    fn fsync(&mut self) -> Result<(), FsyncError> {
        self.gpu.lock().flush().map_err(|_| FsyncError::Failed)?;
        Ok(())
    }

    fn ioctl(&mut self, request: IoctlRequest, arg: &mut [u8]) -> Result<usize, IoctlError> {
        match request {
            IoctlRequest::FbGetScreenInfo => {
                let Ok(arg) = <&mut [u8; FbScreenInfo::SIZE]>::try_from(arg) else {
                    return Err(IoctlError::InvalidArgument);
                };
                let info = FbScreenInfo {
                    width: self.width,
                    height: self.height,
                    pitch: self.width * 4,
                    bpp: 32,
                };
                *arg = info.to_bytes();
                Ok(0)
            }
        }
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
