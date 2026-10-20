use kernel_vfs::{ReadError, Stat, StatError, WriteError};

use crate::DevFile;

#[derive(Debug, Copy, Clone)]
pub struct Null;

impl DevFile for Null {
    fn read(&mut self, _: &mut [u8], _: usize) -> Result<usize, ReadError> {
        Ok(0)
    }

    fn write(&mut self, buf: &[u8], _: usize) -> Result<usize, WriteError> {
        Ok(buf.len())
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_is_always_at_eof() {
        let mut buf = [0xFF_u8; 4];
        assert_eq!(Ok(0), Null.read(&mut buf, 0));
        assert_eq!([0xFF_u8; 4], buf);
    }
}
