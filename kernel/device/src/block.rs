use alloc::boxed::Box;
use alloc::sync::Arc;
use core::error::Error;
use core::ops::{Deref, DerefMut};

use spin::RwLock;

use crate::{Device, DeviceId};

#[repr(transparent)]
pub struct BlockBuf<const N: usize> {
    data: [u8; N],
}

impl<const N: usize> Default for BlockBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> BlockBuf<N> {
    #[must_use]
    pub fn new() -> Self {
        Self { data: [0; N] }
    }
}

impl<const N: usize> Deref for BlockBuf<N> {
    type Target = [u8; N];

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<const N: usize> DerefMut for BlockBuf<N> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

#[allow(clippy::missing_errors_doc)] // TODO: add documentation
pub trait BlockDevice<Id: DeviceId, const N: usize = 512>: Device<Id> {
    fn block_count(&self) -> usize;

    fn read_block(&mut self, block_num: usize, buf: &mut BlockBuf<N>)
    -> Result<(), Box<dyn Error>>;

    fn write_block(&mut self, block_num: usize, buf: &BlockBuf<N>) -> Result<(), Box<dyn Error>>;

    fn flush(&mut self) -> Result<(), Box<dyn Error>>;
}

impl<T, Id> Device<Id> for Arc<RwLock<T>>
where
    T: BlockDevice<Id, 512> + Device<Id>,
    Id: DeviceId,
{
    fn id(&self) -> Id {
        self.read().id()
    }
}

impl<T, Id> BlockDevice<Id, 512> for Arc<RwLock<T>>
where
    T: Device<Id> + BlockDevice<Id, 512>,
    Id: DeviceId,
{
    fn block_count(&self) -> usize {
        self.read().block_count()
    }

    fn read_block(
        &mut self,
        block_num: usize,
        buf: &mut BlockBuf<512>,
    ) -> Result<(), Box<dyn Error>> {
        self.write().read_block(block_num, buf)
    }

    fn write_block(&mut self, block_num: usize, buf: &BlockBuf<512>) -> Result<(), Box<dyn Error>> {
        self.write().write_block(block_num, buf)
    }

    fn flush(&mut self) -> Result<(), Box<dyn Error>> {
        todo!()
    }
}

impl<Id, const N: usize> Device<Id> for Box<dyn BlockDevice<Id, N>>
where
    Id: DeviceId,
{
    fn id(&self) -> Id {
        self.deref().id()
    }
}

impl<Id, const N: usize> BlockDevice<Id, N> for Box<dyn BlockDevice<Id, N>>
where
    Id: DeviceId,
{
    fn block_count(&self) -> usize {
        self.deref().block_count()
    }

    fn read_block(
        &mut self,
        block_num: usize,
        buf: &mut BlockBuf<N>,
    ) -> Result<(), Box<dyn Error>> {
        self.deref_mut().read_block(block_num, buf)
    }

    fn write_block(&mut self, block_num: usize, buf: &BlockBuf<N>) -> Result<(), Box<dyn Error>> {
        self.deref_mut().write_block(block_num, buf)
    }

    fn flush(&mut self) -> Result<(), Box<dyn Error>> {
        self.deref_mut().flush()
    }
}
