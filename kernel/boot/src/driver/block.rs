use alloc::collections::BTreeMap;
use alloc::format;
use alloc::sync::Arc;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;

use kernel_devfs::BlockDeviceFile;
use kernel_device::RegisterDeviceError;
use kernel_device::block::BlockDevice;
use kernel_vfs::path::AbsoluteOwnedPath;
use spin::RwLock;

use crate::driver::KernelDeviceId;
use crate::file::devfs::devfs;

#[allow(clippy::type_complexity)] // refactoring into types doesn't provide benefits here
static BLOCK_DEVICES: RwLock<
    BTreeMap<u64, Arc<RwLock<dyn BlockDevice<KernelDeviceId, 512> + Send + Sync>>>,
> = RwLock::new(BTreeMap::new());
static BLOCK_DEVICE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct BlockDevices;

impl BlockDevices {
    // TODO: add documentation
    #[allow(clippy::missing_errors_doc)]
    #[allow(clippy::missing_panics_doc)]
    pub fn register_block_device<D>(device: Arc<RwLock<D>>) -> Result<(), RegisterDeviceError>
    where
        D: BlockDevice<KernelDeviceId, 512> + Send + Sync + 'static,
    {
        let id = BLOCK_DEVICE_COUNTER.fetch_add(1, Relaxed);
        let _ = BLOCK_DEVICES.write().insert(id, device.clone());

        let path = AbsoluteOwnedPath::try_from(format!("/blk{id}").as_ref()).unwrap();
        devfs()
            .write()
            .register_file(path.as_ref(), {
                move || Ok(BlockDeviceFile::new(device.clone()))
            })
            .unwrap();

        Ok(())
    }

    pub fn by_id(
        id: u64,
    ) -> Option<Arc<RwLock<dyn BlockDevice<KernelDeviceId, 512> + Send + Sync>>> {
        BLOCK_DEVICES.read().get(&id).cloned()
    }
}
