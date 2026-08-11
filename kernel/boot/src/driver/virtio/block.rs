use alloc::boxed::Box;
use alloc::sync::Arc;
use core::alloc::Layout;
use core::error::Error;
use core::fmt::{Debug, Formatter};

use kernel_device::Device;
use kernel_device::block::BlockDevice;
use kernel_memapi::{Guarded, Location, MemoryApi};
use kernel_pci::PciAddress;
use kernel_pci::config::ConfigurationAccess;
use linkme::distributed_slice;
use spin::Mutex;
use spin::rwlock::RwLock;
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::pci::PciTransport;
use x86_64::structures::paging::{PageSize, Size4KiB};

use crate::U64Ext;
use crate::driver::KernelDeviceId;
use crate::driver::block::BlockDevices;
use crate::driver::pci::{PCI_DRIVERS, PciDriverDescriptor, PciDriverType};
use crate::driver::virtio::hal::{HalImpl, transport};
use crate::mem::memapi::{FrameContiguity, HigherHalfAllocation, HigherHalfMemoryApi, Writable};

const PAGE_SIZE: usize = Size4KiB::SIZE as usize;
const SECTOR_SIZE: usize = 512;
const BOUNCE_PAGES: usize = 16;
const BOUNCE_LEN: usize = BOUNCE_PAGES * PAGE_SIZE;
const BOUNCE_SECTORS: usize = BOUNCE_LEN / SECTOR_SIZE;

#[distributed_slice(PCI_DRIVERS)]
static VIRTIO_BLK: PciDriverDescriptor = PciDriverDescriptor {
    name: "virtio-blk",
    typ: PciDriverType::Specific,
    probe: virtio_probe,
    init: virtio_init,
};

fn virtio_probe(addr: PciAddress, cam: &dyn ConfigurationAccess) -> bool {
    addr.vendor_id(cam) == 0x1af4
        && (0x1000..=0x103f).contains(&addr.device_id(cam))
        && addr.subsystem_id(cam) == 0x02
}

#[allow(clippy::needless_pass_by_value)] // signature is required like this
fn virtio_init(addr: PciAddress, cam: Box<dyn ConfigurationAccess>) -> Result<(), Box<dyn Error>> {
    let transport = transport(addr, cam);

    let blk = VirtIOBlk::<HalImpl, _>::new(transport)?;
    let layout = Layout::from_size_align(BOUNCE_LEN, PAGE_SIZE)?;
    let bounce = HigherHalfMemoryApi
        .allocate(
            Location::Anywhere,
            layout,
            FrameContiguity::Contiguous,
            Guarded::No,
        )
        .ok_or(BounceBufferError::AllocationFailed)?;

    let id = KernelDeviceId::new();
    let device = VirtioBlockDevice {
        id,
        inner: Arc::new(Mutex::new(Inner { blk, bounce })),
    };
    let device = Arc::new(RwLock::new(device));
    BlockDevices::register_block_device(device.clone())?;

    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum BounceBufferError {
    #[error("failed to allocate the bounce buffer")]
    AllocationFailed,
    #[error("buffer of {0} bytes exceeds the bounce buffer")]
    BufferTooLarge(usize),
}

struct Inner {
    blk: VirtIOBlk<HalImpl, PciTransport>,
    bounce: HigherHalfAllocation<Writable>,
}

#[derive(Clone)]
pub struct VirtioBlockDevice {
    id: KernelDeviceId,
    inner: Arc<Mutex<Inner>>,
}

impl Debug for VirtioBlockDevice {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VirtioBlockDevice")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Device<KernelDeviceId> for VirtioBlockDevice {
    fn id(&self) -> KernelDeviceId {
        self.id
    }
}

impl BlockDevice for VirtioBlockDevice {
    type Error = Box<dyn Error>;

    fn sector_size(&self) -> usize {
        SECTOR_SIZE
    }

    fn sector_count(&self) -> usize {
        self.inner.lock().blk.capacity().into_usize()
    }

    fn read_sector(&self, sector_index: usize, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let Inner { blk, bounce } = &mut *self.inner.lock();
        let staging = bounce
            .as_mut()
            .get_mut(..buf.len())
            .ok_or(BounceBufferError::BufferTooLarge(buf.len()))?;
        blk.read_blocks(sector_index, staging)?;
        buf.copy_from_slice(staging);
        Ok(buf.len())
    }

    fn write_sector(&mut self, sector_index: usize, buf: &[u8]) -> Result<usize, Self::Error> {
        let Inner { blk, bounce } = &mut *self.inner.lock();
        let staging = bounce
            .as_mut()
            .get_mut(..buf.len())
            .ok_or(BounceBufferError::BufferTooLarge(buf.len()))?;
        staging.copy_from_slice(buf);
        blk.write_blocks(sector_index, staging)?;
        Ok(buf.len())
    }

    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        let Inner { blk, bounce } = &mut *self.inner.lock();

        let mut position = offset;
        let mut done = 0;
        while done < buf.len() {
            let sector_index = position / SECTOR_SIZE;
            let intra_sector = position % SECTOR_SIZE;
            let remaining = buf.len() - done;
            let sectors = (intra_sector + remaining)
                .div_ceil(SECTOR_SIZE)
                .min(BOUNCE_SECTORS);

            let len = sectors * SECTOR_SIZE;
            let staging = bounce
                .as_mut()
                .get_mut(..len)
                .ok_or(BounceBufferError::BufferTooLarge(len))?;
            blk.read_blocks(sector_index, staging)?;

            let take = remaining.min(staging.len() - intra_sector);
            buf[done..done + take].copy_from_slice(&staging[intra_sector..intra_sector + take]);

            done += take;
            position += take;
        }

        Ok(buf.len())
    }
}
