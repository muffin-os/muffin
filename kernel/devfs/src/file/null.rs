use kernel_vfs::{ReadError, Stat, StatError, WriteError};

use crate::DevFile;

#[derive(Debug, Copy, Clone)]
pub struct Null;

impl DevFile for Null {
    fn read(&mut self, _: &mut [u8], _: usize) -> Result<usize, ReadError> {
        Err(ReadError::EndOfFile)
    }

    fn write(&mut self, buf: &[u8], _: usize) -> Result<usize, WriteError> {
        Ok(buf.len())
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = 0;
        Ok(())
    }
}
