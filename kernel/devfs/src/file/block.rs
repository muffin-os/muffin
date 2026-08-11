use kernel_device::block::BlockDevice;
use kernel_vfs::{ReadError, Stat, StatError, WriteError};

use crate::DevFile;

pub struct BlockDeviceFile<D: BlockDevice + Send + Sync> {
    device: D,
}

impl<D: BlockDevice + Send + Sync> BlockDeviceFile<D> {
    pub fn new(device: D) -> Self {
        Self { device }
    }
}

impl<D: BlockDevice + Send + Sync> DevFile for BlockDeviceFile<D> {
    fn read(&mut self, buf: &mut [u8], offset: usize) -> Result<usize, ReadError> {
        self.device
            .read_at(offset, buf)
            .map_err(|_| ReadError::ReadFailed)
    }

    fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        self.device
            .write_at(offset, buf)
            .map_err(|_| WriteError::WriteFailed)
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        *stat = Stat {
            size: self.device.sector_count() * self.device.sector_size(),
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::error::Error;

    use super::*;

    const SECTOR_SIZE: usize = 10;
    const SECTOR_COUNT: usize = 5;
    const DEVICE_SIZE: usize = SECTOR_SIZE * SECTOR_COUNT;

    struct MockDevice {
        data: Vec<u8>,
    }

    impl MockDevice {
        fn new() -> Self {
            Self {
                data: (0..u8::MAX).cycle().take(DEVICE_SIZE).collect(),
            }
        }

        fn sector_start(&self, sector_index: usize) -> Result<usize, Box<dyn Error>> {
            if sector_index >= SECTOR_COUNT {
                return Err(Box::new(ReadError::ReadFailed));
            }
            Ok(sector_index * SECTOR_SIZE)
        }
    }

    impl BlockDevice for MockDevice {
        type Error = Box<dyn Error>;

        fn sector_size(&self) -> usize {
            SECTOR_SIZE
        }

        fn sector_count(&self) -> usize {
            SECTOR_COUNT
        }

        fn read_sector(&self, sector_index: usize, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let start = self.sector_start(sector_index)?;
            buf.copy_from_slice(&self.data[start..start + SECTOR_SIZE]);
            Ok(buf.len())
        }

        fn write_sector(&mut self, sector_index: usize, buf: &[u8]) -> Result<usize, Self::Error> {
            let start = self.sector_start(sector_index)?;
            self.data[start..start + SECTOR_SIZE].copy_from_slice(buf);
            Ok(buf.len())
        }
    }

    fn file() -> BlockDeviceFile<MockDevice> {
        BlockDeviceFile::new(MockDevice::new())
    }

    fn expected(offset: usize, len: usize) -> Vec<u8> {
        MockDevice::new().data[offset..offset + len].to_vec()
    }

    #[test]
    fn read_covers_every_offset_and_length() {
        let mut file = file();

        for offset in 0..DEVICE_SIZE {
            for len in 0..=DEVICE_SIZE - offset {
                let mut buf = vec![0_u8; len];
                let read = file.read(&mut buf, offset).unwrap();
                assert_eq!(len, read, "read {len} bytes at offset {offset}");
                assert_eq!(expected(offset, len), buf, "data at offset {offset}");
            }
        }
    }

    #[test]
    fn write_covers_every_offset_and_length() {
        for offset in 0..DEVICE_SIZE {
            for len in 0..=DEVICE_SIZE - offset {
                let mut file = file();
                let buf = vec![0xAA_u8; len];
                let written = file.write(&buf, offset).unwrap();
                assert_eq!(len, written, "wrote {len} bytes at offset {offset}");

                let data = &file.device.data;
                assert_eq!(
                    expected(0, offset),
                    data[..offset],
                    "bytes before offset {offset} were modified"
                );
                assert_eq!(buf, data[offset..offset + len], "written bytes at {offset}");
                assert_eq!(
                    expected(offset + len, DEVICE_SIZE - offset - len),
                    data[offset + len..],
                    "bytes after offset {offset} plus {len} were modified"
                );
            }
        }
    }

    #[test]
    fn read_past_device_end_fails() {
        let mut file = file();

        let mut buf = vec![0_u8; 1];
        assert_eq!(Err(ReadError::ReadFailed), file.read(&mut buf, DEVICE_SIZE));

        let mut buf = vec![0_u8; SECTOR_SIZE];
        assert_eq!(Err(ReadError::ReadFailed), file.read(&mut buf, DEVICE_SIZE));

        let mut buf = vec![0_u8; DEVICE_SIZE + 1];
        assert_eq!(Err(ReadError::ReadFailed), file.read(&mut buf, 0));

        let mut buf = vec![0_u8; 2];
        assert_eq!(
            Err(ReadError::ReadFailed),
            file.read(&mut buf, DEVICE_SIZE - 1)
        );
    }

    #[test]
    fn write_past_device_end_fails() {
        let mut file = file();

        assert_eq!(Err(WriteError::WriteFailed), file.write(&[1], DEVICE_SIZE));

        let buf = vec![1_u8; SECTOR_SIZE];
        assert_eq!(
            Err(WriteError::WriteFailed),
            file.write(&buf, DEVICE_SIZE),
            "aligned write one sector past the end"
        );

        let buf = vec![1_u8; DEVICE_SIZE + 1];
        assert_eq!(Err(WriteError::WriteFailed), file.write(&buf, 0));

        assert_eq!(
            Err(WriteError::WriteFailed),
            file.write(&[1, 2], DEVICE_SIZE - 1)
        );
    }

    #[test]
    fn empty_transfers_touch_nothing() {
        let mut file = file();

        assert_eq!(Ok(0), file.read(&mut [], 0));
        assert_eq!(Ok(0), file.read(&mut [], DEVICE_SIZE));
        assert_eq!(Ok(0), file.write(&[], 0));
        assert_eq!(Ok(0), file.write(&[], DEVICE_SIZE));
        assert_eq!(expected(0, DEVICE_SIZE), file.device.data);
    }

    #[test]
    fn stat_reports_device_size() {
        let mut file = file();

        let mut stat = Stat::default();
        file.stat(&mut stat).unwrap();
        assert_eq!(DEVICE_SIZE, stat.size);
    }
}
