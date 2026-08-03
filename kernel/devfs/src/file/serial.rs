use core::fmt::Write;
use core::str::from_utf8;

use kernel_vfs::{ReadError, Stat, StatError, WriteError};

use crate::DevFile;

pub struct Serial<T> {
    out: T,
}

impl<T> Default for Serial<T>
where
    T: Default,
{
    fn default() -> Self {
        Self { out: T::default() }
    }
}

impl<T> DevFile for Serial<T>
where
    T: Write + Send + Sync,
{
    fn read(&mut self, _: &mut [u8], _: usize) -> Result<usize, ReadError> {
        Err(ReadError::EndOfFile)
    }

    fn write(&mut self, buf: &[u8], _: usize) -> Result<usize, WriteError> {
        let s = from_utf8(buf).map_err(|_| WriteError::WriteFailed)?;
        self.out.write_str(s).map_err(|_| WriteError::WriteFailed)?;
        Ok(buf.len())
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = 0;
        Ok(())
    }
}
