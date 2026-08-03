#![no_std]
extern crate alloc;

use core::fmt::Display;

use crate::config::{ConfigKey, ReadConfig};

pub mod config;

/// The description of a pci address consisting of bus, device and function.
/// A pci address does not imply that a device is present at that address.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Display for PciAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}.{:01x}",
            self.bus, self.device, self.function
        )
    }
}

macro_rules! getter {
    ($name:ident : $typ:ty = $key:expr) => {
        pub fn $name<C: ReadConfig<$typ> + ?Sized>(&self, config: &C) -> $typ {
            config.read_config(*self, $key)
        }
    };
}

impl PciAddress {
    pub fn new(bus: u8, device: u8, function: u8) -> Self {
        Self {
            bus,
            device,
            function,
        }
    }

    getter!(vendor_id: u16 = ConfigKey::VENDOR_ID);
    getter!(device_id: u16 = ConfigKey::DEVICE_ID);
    getter!(header_type: u8 = ConfigKey::HEADER_TYPE);
    getter!(bar0: u32 = ConfigKey::BAR0);
    getter!(bar1: u32 = ConfigKey::BAR1);
    getter!(bar2: u32 = ConfigKey::BAR2);
    getter!(bar3: u32 = ConfigKey::BAR3);
    getter!(bar4: u32 = ConfigKey::BAR4);
    getter!(bar5: u32 = ConfigKey::BAR5);
    getter!(subsystem_id: u16 = ConfigKey::SUBSYSTEM_ID);

    pub fn is_multifunction<C: ReadConfig<u8>>(&self, config: &C) -> bool {
        self.header_type(config) & 0x80 != 0
    }
}
