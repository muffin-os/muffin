use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use core::error::Error;

use kernel_pci::PciAddress;
use kernel_pci::config::{ConfigKey, ConfigurationAccess, PortCam, ReadConfig, WriteConfig};
use linkme::distributed_slice;
use log::{Level, debug, error, log_enabled, trace};
use virtio_drivers::transport::pci::bus::DeviceFunction;

#[distributed_slice]
pub static PCI_DRIVERS: [PciDriverDescriptor] = [..];

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PciDriverType {
    Generic,
    Specific,
}

#[allow(clippy::type_complexity)] // refactoring the `init` fn type doesn't provide benefits here
pub struct PciDriverDescriptor {
    pub name: &'static str,
    pub typ: PciDriverType,
    pub probe: fn(PciAddress, &dyn ConfigurationAccess) -> bool,
    pub init: fn(PciAddress, Box<dyn ConfigurationAccess>) -> Result<(), Box<dyn Error>>,
}

/// # Panics
///
/// Panics if there are multiple specific or multiple generic drivers that would match
/// the same device.
pub fn init() {
    if log_enabled!(Level::Trace) {
        PCI_DRIVERS
            .iter()
            .for_each(|driver| trace!("have pci driver: {}", driver.name));
    }

    let cam = unsafe { PortCam::new() };

    unsafe { iterate_all(&cam) }.for_each(|addr| {
        let driver = PCI_DRIVERS
            .iter()
            .fold(None, |res: Option<&PciDriverDescriptor>, driver| {
                if !(driver.probe)(addr, &cam) {
                    return res;
                }

                if let Some(other_driver) = res {
                    if other_driver.typ == PciDriverType::Generic
                        && driver.typ == PciDriverType::Specific
                    {
                        return Some(driver);
                    } else if other_driver.typ == PciDriverType::Specific
                        && driver.typ == PciDriverType::Generic
                    {
                        return Some(other_driver);
                    }

                    panic!(
                        "found two drivers for the same device: {} and {}",
                        other_driver.name, driver.name
                    );
                } else {
                    Some(driver)
                }
            });
        if let Some(driver) = driver {
            debug!("found driver {} for device {}", driver.name, addr);
            let device_string = addr.to_string();
            if let Err(e) = (driver.init)(addr, Box::new(cam.clone())) {
                error!(
                    "failed to init driver {} for device {}: {}",
                    driver.name, device_string, e
                );
            }
        }
    });
}

unsafe fn iterate_all<C: ConfigurationAccess>(cam: &C) -> impl Iterator<Item = PciAddress> {
    (0..=u8::MAX)
        .flat_map(|bus| (0_u8..32).map(move |slot| (bus, slot)))
        .flat_map(|(bus, slot)| {
            let addr = PciAddress::new(bus, slot, 0);

            if addr.vendor_id(cam) == 0xFFFF {
                0_u8..0
            } else if addr.is_multifunction(cam) {
                0_u8..8
            } else {
                0_u8..1
            }
            .map(move |function| PciAddress::new(bus, slot, function))
        })
}

pub struct VirtIoCam(Arc<Box<dyn ConfigurationAccess>>);

impl VirtIoCam {
    pub fn new(cam: Box<dyn ConfigurationAccess>) -> Self {
        Self(Arc::new(cam))
    }
}

impl virtio_drivers::transport::pci::bus::ConfigurationAccess for VirtIoCam {
    fn read_word(&self, device_function: DeviceFunction, register_offset: u8) -> u32 {
        self.0.read_config(
            PciAddress::new(
                device_function.bus,
                device_function.device,
                device_function.function,
            ),
            ConfigKey::<u32>::try_from(register_offset as usize).unwrap(),
        )
    }

    fn write_word(&mut self, device_function: DeviceFunction, register_offset: u8, data: u32) {
        self.0.write_config(
            PciAddress::new(
                device_function.bus,
                device_function.device,
                device_function.function,
            ),
            ConfigKey::<u32>::try_from(register_offset as usize).unwrap(),
            data,
        )
    }

    unsafe fn unsafe_clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}
