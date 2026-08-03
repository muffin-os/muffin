#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use core::ops::Deref;

use thiserror::Error;

pub mod block;
pub mod raw;

pub trait Device<Id: DeviceId> {
    fn id(&self) -> Id;
}

impl<Id> Device<Id> for Box<dyn Device<Id>>
where
    Id: DeviceId,
{
    fn id(&self) -> Id {
        self.deref().id()
    }
}

pub trait DeviceId: Copy + Eq {}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
pub enum RegisterDeviceError {
    #[error("device id is already registered")]
    AlreadyRegistered,
}
