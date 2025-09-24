use alloc::boxed::Box;
use core::marker::PhantomData;

use crate::PciAddress;

#[cfg(target_arch = "x86_64")]
mod port;
#[cfg(target_arch = "x86_64")]
pub use port::*;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct ConfigKey<T> {
    offset: u8,
    _t: PhantomData<T>,
}

impl<T> ConfigKey<T> {
    const fn new(offset: u8) -> ConfigKey<T> {
        ConfigKey {
            offset,
            _t: PhantomData,
        }
    }
}

impl TryFrom<usize> for ConfigKey<u8> {
    type Error = usize;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value > u8::MAX as usize {
            Err(value)
        } else {
            Ok(ConfigKey::new(value as u8))
        }
    }
}

impl TryFrom<usize> for ConfigKey<u16> {
    type Error = usize;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value & 1 != 0 {
            return Err(value);
        }

        if value > u16::MAX as usize {
            Err(value)
        } else {
            Ok(ConfigKey::new(value as u8))
        }
    }
}

impl TryFrom<usize> for ConfigKey<u32> {
    type Error = usize;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value & 3 != 0 {
            return Err(value);
        }

        if value > u32::MAX as usize {
            Err(value)
        } else {
            Ok(ConfigKey::new(value as u8))
        }
    }
}

macro_rules! config_key {
    ($name:ident, $typ:ty, $offset:expr) => {
        impl ConfigKey<$typ> {
            pub const $name: Self = ConfigKey::new($offset);
        }
    };
}

config_key!(VENDOR_ID, u16, 0x00);
config_key!(DEVICE_ID, u16, 0x02);
config_key!(COMMAND, u16, 0x04);
config_key!(STATUS, u16, 0x06);
config_key!(REVISION_ID, u8, 0x08);
config_key!(PROG_IF, u8, 0x09);
config_key!(SUBCLASS, u8, 0x0A);
config_key!(CLASS, u8, 0x0B);
config_key!(CACHE_LINE_SIZE, u8, 0x0C);
config_key!(LATENCY_TIMER, u8, 0x0D);
config_key!(HEADER_TYPE, u8, 0x0E);
config_key!(BIST, u8, 0x0F);
config_key!(BAR0, u32, 0x10);
config_key!(BAR1, u32, 0x14);
config_key!(BAR2, u32, 0x18);
config_key!(BAR3, u32, 0x1C);
config_key!(BAR4, u32, 0x20);
config_key!(BAR5, u32, 0x24);
config_key!(CARDBUS_CIS_POINTER, u32, 0x28);
config_key!(SUBSYSTEM_VENDOR_ID, u16, 0x2C);
config_key!(SUBSYSTEM_ID, u16, 0x2E);
config_key!(EXPANSION_ROM_BASE_ADDRESS, u32, 0x30);
config_key!(CAPABILITIES_POINTER, u8, 0x34);
config_key!(INTERRUPT_LINE, u8, 0x3C);
config_key!(INTERRUPT_PIN, u8, 0x3D);
config_key!(MIN_GRANT, u8, 0x3E);
config_key!(MAX_LATENCY, u8, 0x3F);

pub trait ReadConfigs: ReadConfig<u8> + ReadConfig<u16> + ReadConfig<u32> {}

impl<T> ReadConfigs for T where Self: ReadConfig<u8> + ReadConfig<u16> + ReadConfig<u32> {}

pub trait ReadConfig<T>: Send + Sync {
    fn read_config(&self, addr: PciAddress, config: ConfigKey<T>) -> T;
}

impl<T, U> ReadConfig<T> for Box<U>
where
    U: ReadConfig<T> + ?Sized,
{
    fn read_config(&self, addr: PciAddress, config: ConfigKey<T>) -> T {
        U::read_config(self, addr, config)
    }
}

impl<T> ReadConfig<T> for &mut dyn ReadConfig<T> {
    fn read_config(&self, addr: PciAddress, config: ConfigKey<T>) -> T {
        (**self).read_config(addr, config)
    }
}

pub trait WriteConfigs: WriteConfig<u8> + WriteConfig<u16> + WriteConfig<u32> {}

impl<T> WriteConfigs for T where Self: WriteConfig<u8> + WriteConfig<u16> + WriteConfig<u32> {}

pub trait WriteConfig<T>: Send + Sync {
    fn write_config(&self, addr: PciAddress, config: ConfigKey<T>, value: T);
}

impl<T, U> WriteConfig<T> for Box<U>
where
    U: WriteConfig<T> + ?Sized,
{
    fn write_config(&self, addr: PciAddress, config: ConfigKey<T>, value: T) {
        U::write_config(self, addr, config, value)
    }
}

impl<T> WriteConfig<T> for &mut dyn WriteConfig<T> {
    fn write_config(&self, addr: PciAddress, config: ConfigKey<T>, value: T) {
        (**self).write_config(addr, config, value)
    }
}

pub trait ConfigurationAccess: ReadConfigs + WriteConfigs {}

impl<T> ConfigurationAccess for T where Self: ReadConfigs + WriteConfigs {}

const _: () = {
    const fn _assert_configs<T: ConfigurationAccess>() {}
    _assert_configs::<Box<dyn ConfigurationAccess>>();
};
