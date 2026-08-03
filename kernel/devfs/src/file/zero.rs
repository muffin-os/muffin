use kernel_vfs::{ReadError, Stat, StatError, WriteError};

use crate::DevFile;

#[derive(Debug, Copy, Clone)]
pub struct Zero;

impl DevFile for Zero {
    fn read(&mut self, buf: &mut [u8], _: usize) -> Result<usize, ReadError> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write(&mut self, buf: &[u8], _: usize) -> Result<usize, WriteError> {
        Ok(buf.len())
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = 0;
        Ok(())
    }
}
