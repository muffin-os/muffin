use alloc::sync::Arc;

use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::PciAddress;
use crate::config::{ConfigKey, ReadConfig, WriteConfig};

#[derive(Debug, Clone)]
pub struct PortCam {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    address: Port<u32>,
    data: Port<u32>,
}

impl PortCam {
    /// # Safety
    /// The caller must ensure that at most one instance exists at a time.
    pub unsafe fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                address: Port::new(0xCF8),
                data: Port::new(0xCFC),
            })),
        }
    }
}

impl ReadConfig<u8> for PortCam
where
    Self: ReadConfig<u32>,
{
    fn read_config(&self, addr: PciAddress, config: ConfigKey<u8>) -> u8 {
        let shif = (config.offset & 3) * 8;
        let config = ConfigKey::<u32>::new(config.offset & !3);
        let value = self.read_config(addr, config);
        (value >> shif) as u8
    }
}

impl ReadConfig<u16> for PortCam
where
    Self: ReadConfig<u32>,
{
    fn read_config(&self, addr: PciAddress, config: ConfigKey<u16>) -> u16 {
        #[cfg(debug_assertions)]
        assert_eq!(config.offset & 1, 0, "16bit reads must be 16bit aligned");

        let shift = (config.offset & 2) * 8;
        let config = ConfigKey::<u32>::new(config.offset & !3);
        let value = self.read_config(addr, config);
        (value >> shift) as u16
    }
}

impl ReadConfig<u32> for PortCam {
    fn read_config(&self, addr: PciAddress, config: ConfigKey<u32>) -> u32 {
        #[cfg(debug_assertions)]
        assert_eq!(config.offset & 3, 0, "all reads must be 32bit aligned");
        let address = (1 << 31)
            | (u32::from(addr.bus) << 16)
            | (u32::from(addr.device) << 11)
            | (u32::from(addr.function) << 8)
            | u32::from(config.offset);

        let mut guard = self.inner.lock();
        unsafe {
            guard.address.write(address);
            guard.data.read()
        }
    }
}

impl WriteConfig<u8> for PortCam
where
    Self: WriteConfig<u32>,
{
    fn write_config(&self, addr: PciAddress, config: ConfigKey<u8>, value: u8) {
        let shift = (config.offset & 3) * 8;
        let config = ConfigKey::<u32>::new(config.offset & !3);
        let mut read = self.read_config(addr, config);
        read &= !(0xFF << shift);
        read |= u32::from(value) << shift;
        self.write_config(addr, config, read);
    }
}

impl WriteConfig<u16> for PortCam
where
    Self: WriteConfig<u32> + ReadConfig<u16>,
{
    fn write_config(&self, addr: PciAddress, config: ConfigKey<u16>, value: u16) {
        #[cfg(debug_assertions)]
        assert_eq!(config.offset & 1, 0, "16bit writes must be 16bit aligned");

        let shift = (config.offset & 2) * 8;
        let config = ConfigKey::<u32>::new(config.offset & !3);
        let mut read = self.read_config(addr, config);
        read &= !(0xFFFF << shift);
        read |= u32::from(value) << shift;
        self.write_config(addr, config, read);
    }
}

impl WriteConfig<u32> for PortCam {
    fn write_config(&self, addr: PciAddress, config: ConfigKey<u32>, value: u32) {
        #[cfg(debug_assertions)]
        assert_eq!(config.offset & 3, 0, "all writes must be 32bit aligned");
        let address = (1 << 31)
            | (u32::from(addr.bus) << 16)
            | (u32::from(addr.device) << 11)
            | (u32::from(addr.function) << 8)
            | u32::from(config.offset);

        let mut guard = self.inner.lock();
        unsafe {
            guard.address.write(address);
            guard.data.write(value)
        };
    }
}
