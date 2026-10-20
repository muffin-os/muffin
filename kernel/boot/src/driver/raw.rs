use alloc::sync::Arc;

use kernel_device::RegisterDeviceError;
use kernel_device::raw::{RawDevice, RawDeviceRegistry};
use spin::RwLock;

use crate::driver::KernelDeviceId;

static RAW_DEVICES: RwLock<RawDeviceRegistry<KernelDeviceId>> =
    RwLock::new(RawDeviceRegistry::new());

pub struct RawDevices;

impl RawDevices {
    /// # Errors
    /// Returns `AlreadyRegistered` if a device with the same id is registered.
    pub fn register_raw_device<D>(device: Arc<RwLock<D>>) -> Result<(), RegisterDeviceError>
    where
        D: RawDevice<KernelDeviceId> + Send + Sync + 'static,
    {
        RAW_DEVICES.write().register_device(device)?;
        // TODO(tpdenk): expose raw devices in devfs the way register_block_device does
        Ok(())
    }
}
