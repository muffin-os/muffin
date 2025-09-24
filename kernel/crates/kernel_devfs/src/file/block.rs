use core::marker::PhantomData;

use kernel_device::DeviceId;
use kernel_device::block::{BlockBuf, BlockDevice};
use kernel_vfs::{ReadError, Stat, StatError, WriteError};

use crate::DevFile;

pub struct BlockDeviceFile<D, I, const N: usize> {
    device: D,
    id: PhantomData<I>,
}

impl<D, I, const N: usize> BlockDeviceFile<D, I, N> {
    pub fn new(inner: D) -> Self {
        Self {
            device: inner,
            id: PhantomData,
        }
    }
}
impl<D, I, const N: usize> BlockDeviceFile<D, I, N>
where
    D: BlockDevice<I, N> + Send + Sync,
    I: DeviceId + Send + Sync,
{
    fn block_containing_offset(offset: usize) -> usize {
        (offset + 1).div_ceil(N) - 1
    }

    #[cfg(test)]
    fn into_device(self) -> D {
        self.device
    }
}

impl<D, I, const N: usize> DevFile for BlockDeviceFile<D, I, N>
where
    D: BlockDevice<I, N> + Send + Sync,
    I: DeviceId + Send + Sync,
{
    fn read(&mut self, buf: &mut [u8], offset: usize) -> Result<usize, ReadError> {
        let block_size = N;
        let block_count = self.device.block_count();
        let buf_len = buf.len();
        if buf_len == 0 {
            return Ok(0);
        }

        let first_block_relative_offset = offset % block_size;
        let start_block = Self::block_containing_offset(offset);
        if start_block >= block_count {
            return Err(ReadError::ReadFailed);
        }
        let end_block_inclusive = Self::block_containing_offset(offset + buf.len() - 1);
        if start_block > end_block_inclusive {
            return Err(ReadError::ReadFailed);
        }

        // first block read
        let bytes_to_copy = (N - first_block_relative_offset).min(buf_len);
        if first_block_relative_offset == 0 {
            // aligned read
            let mut read_buf = BlockBuf::new();
            self.device
                .read_block(start_block, &mut read_buf)
                .map_err(|_| ReadError::ReadFailed)?;
            buf[..bytes_to_copy].copy_from_slice(&read_buf[..bytes_to_copy]);
        } else {
            // first block is unaligned read
            let mut read_buf = BlockBuf::new();
            self.device
                .read_block(start_block, &mut read_buf)
                .map_err(|_| ReadError::ReadFailed)?;
            buf[..bytes_to_copy].copy_from_slice(
                &read_buf[first_block_relative_offset..first_block_relative_offset + bytes_to_copy],
            );
        }

        if start_block == end_block_inclusive {
            // if we only read one block, we are done
            return Ok(bytes_to_copy);
        }

        // middle blocks read
        for block in start_block + 1..end_block_inclusive {
            let mut read_buf = BlockBuf::new();
            self.device
                .read_block(block, &mut read_buf)
                .map_err(|_| ReadError::ReadFailed)?;

            let buf_offset = (block - start_block) * N - first_block_relative_offset;
            let bytes_to_copy = (buf_len - buf_offset).min(N);
            buf[buf_offset..buf_offset + bytes_to_copy].copy_from_slice(&read_buf[..bytes_to_copy]);
        }

        // end block read
        let mut read_buf = BlockBuf::new();
        self.device
            .read_block(end_block_inclusive, &mut read_buf)
            .map_err(|_| ReadError::ReadFailed)?;

        let buf_offset = (end_block_inclusive - start_block) * N - first_block_relative_offset;
        let bytes_to_copy = (buf_len - buf_offset).min(N);
        buf[buf_offset..buf_offset + bytes_to_copy].copy_from_slice(&read_buf[..bytes_to_copy]);

        Ok(buf_offset + bytes_to_copy)
    }

    fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize, WriteError> {
        let block_size = N;
        let block_count = self.device.block_count();
        let buf_len = buf.len();
        if buf_len == 0 {
            return Ok(0);
        }

        let first_block_relative_offset = offset % block_size;
        let start_block = Self::block_containing_offset(offset);
        if start_block >= block_count {
            return Err(WriteError::WriteFailed);
        }
        let end_block_inclusive = Self::block_containing_offset(offset + buf.len() - 1);
        if start_block > end_block_inclusive {
            return Err(WriteError::WriteFailed);
        }

        // first block write
        let bytes_to_copy = (N - first_block_relative_offset).min(buf_len);
        if first_block_relative_offset == 0 && buf_len >= N {
            // aligned write
            let mut write_buf = BlockBuf::new();
            write_buf[..bytes_to_copy].copy_from_slice(&buf[..bytes_to_copy]);
            self.device
                .write_block(start_block, &write_buf)
                .map_err(|_| WriteError::WriteFailed)?;
        } else {
            // first block is unaligned write or short buf
            let mut read_buf = BlockBuf::new();
            self.device
                .read_block(start_block, &mut read_buf)
                .map_err(|_| WriteError::WriteFailed)?;
            read_buf[first_block_relative_offset..first_block_relative_offset + bytes_to_copy]
                .copy_from_slice(&buf[..bytes_to_copy]);
            self.device
                .write_block(start_block, &read_buf)
                .map_err(|_| WriteError::WriteFailed)?;
        }

        if start_block == end_block_inclusive {
            // if we only write one block, we are done
            return Ok(bytes_to_copy);
        }

        // middle blocks write
        for block in start_block + 1..end_block_inclusive {
            let mut write_buf = BlockBuf::new();
            let buf_offset = (block - start_block) * N - first_block_relative_offset;
            let bytes_to_copy = (buf_len - buf_offset).min(N);
            write_buf[..bytes_to_copy]
                .copy_from_slice(&buf[buf_offset..buf_offset + bytes_to_copy]);
            self.device
                .write_block(block, &write_buf)
                .map_err(|_| WriteError::WriteFailed)?;
        }

        // end block write
        let mut write_buf = BlockBuf::new();
        let buf_offset = (end_block_inclusive - start_block) * N - first_block_relative_offset;
        let bytes_to_copy = (buf_len - buf_offset).min(N);
        if bytes_to_copy < N {
            self.device
                .read_block(end_block_inclusive, &mut write_buf)
                .map_err(|_| WriteError::WriteFailed)?;
        }
        write_buf[..bytes_to_copy].copy_from_slice(&buf[buf_offset..buf_offset + bytes_to_copy]);
        self.device
            .write_block(end_block_inclusive, &write_buf)
            .map_err(|_| WriteError::WriteFailed)?;

        Ok(buf_offset + bytes_to_copy)
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        *stat = Stat { size: stat.size };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::error::Error;

    use kernel_device::Device;

    use super::*;

    #[derive(Debug, Copy, Clone, Eq, PartialEq)]
    struct TestDeviceId(usize);
    impl DeviceId for TestDeviceId {}

    struct TestBlockDevice<const N: usize> {
        id: TestDeviceId,
        data: Vec<u8>,
    }

    impl<const N: usize> TestBlockDevice<N> {
        fn into_data(self) -> Vec<u8> {
            self.data
        }
    }

    impl<const N: usize> Device<TestDeviceId> for TestBlockDevice<N> {
        fn id(&self) -> TestDeviceId {
            self.id
        }
    }

    impl<const N: usize> BlockDevice<TestDeviceId, N> for TestBlockDevice<N> {
        fn block_count(&self) -> usize {
            self.data.len() / N
        }

        fn read_block(
            &mut self,
            block_num: usize,
            buf: &mut BlockBuf<N>,
        ) -> Result<(), Box<dyn Error>> {
            if block_num >= self.block_count() {
                return Err(Box::new(ReadError::ReadFailed));
            }

            let start = block_num * N;
            let end = start + N;

            buf.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write_block(
            &mut self,
            block_num: usize,
            buf: &BlockBuf<N>,
        ) -> Result<(), Box<dyn Error>> {
            if block_num >= self.block_count() {
                return Err(Box::new(ReadError::ReadFailed));
            }

            let start = block_num * N;
            let end = start + N;

            self.data[start..end].copy_from_slice(buf.as_slice());
            Ok(())
        }

        fn flush(&mut self) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
    }

    #[test]
    fn test_block_containing_offset() {
        const BLOCK_SIZE: usize = 10;

        for block in 0..5 {
            for offset in 0..BLOCK_SIZE {
                assert_eq!(
                    block,
                    BlockDeviceFile::<TestBlockDevice<BLOCK_SIZE>,TestDeviceId,BLOCK_SIZE>::block_containing_offset(block * BLOCK_SIZE + offset)
                );
            }
        }
    }

    fn test_write_blocks<const BLOCK_SIZE: usize, const NUM_BLOCKS: usize>(
        buf_len: usize,
        write_offset: usize,
    ) {
        fn data() -> impl Iterator<Item = u8> {
            (1..u8::MAX).cycle()
        }

        let device = TestBlockDevice::<BLOCK_SIZE> {
            id: TestDeviceId(1),
            data: data().take(BLOCK_SIZE * NUM_BLOCKS).collect(),
        };

        let mut file = BlockDeviceFile::new(device);

        let buf = vec![0; buf_len];
        let result = file.write(&buf, write_offset);
        if write_offset >= BLOCK_SIZE * NUM_BLOCKS
            || write_offset + buf_len > BLOCK_SIZE * NUM_BLOCKS
        {
            assert_eq!(result, Err(WriteError::WriteFailed));
            return;
        }

        let bytes_written = result.unwrap();
        assert_eq!(
            bytes_written,
            buf.len().min(BLOCK_SIZE * NUM_BLOCKS - write_offset)
        );

        let device = file.into_device();
        let device_data = device.into_data();

        assert_eq!(
            &device_data[..write_offset],
            &data().take(write_offset).collect::<Vec<_>>()
        );
        assert_eq!(
            &device_data[write_offset..write_offset + bytes_written],
            &buf[..bytes_written]
        );
        assert_eq!(
            &device_data[write_offset + bytes_written..],
            &data()
                .skip(write_offset + bytes_written)
                .take(BLOCK_SIZE * NUM_BLOCKS - write_offset - bytes_written)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_write() {
        const DEVICE_BLOCKS: usize = 5;
        const BLOCK_SIZE: usize = 10;

        for offset in 0..BLOCK_SIZE {
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(BLOCK_SIZE / 2, 0 * BLOCK_SIZE + offset);

            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(2 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(3 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(4 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(5 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);

            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 1 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(2 * BLOCK_SIZE, 1 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(3 * BLOCK_SIZE, 1 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(4 * BLOCK_SIZE, 1 * BLOCK_SIZE + offset);

            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 2 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(2 * BLOCK_SIZE, 2 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(3 * BLOCK_SIZE, 2 * BLOCK_SIZE + offset);

            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 3 * BLOCK_SIZE + offset);
            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(2 * BLOCK_SIZE, 3 * BLOCK_SIZE + offset);

            test_write_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 4 * BLOCK_SIZE + offset);
        }
    }

    fn test_read_blocks<const BLOCK_SIZE: usize, const NUM_BLOCKS: usize>(
        buf_len: usize,
        read_offset: usize,
    ) {
        fn data() -> impl Iterator<Item = u8> {
            (0..u8::MAX).cycle()
        }

        let device = TestBlockDevice::<BLOCK_SIZE> {
            id: TestDeviceId(1),
            data: data().take(BLOCK_SIZE * NUM_BLOCKS).collect(),
        };

        let mut file = BlockDeviceFile::new(device);

        let mut buf = vec![0; buf_len];
        let result = file.read(&mut buf, read_offset);

        if read_offset >= BLOCK_SIZE * NUM_BLOCKS || read_offset + buf_len > BLOCK_SIZE * NUM_BLOCKS
        {
            assert_eq!(result, Err(ReadError::ReadFailed));
            return;
        }

        let bytes_read = result.unwrap();
        assert_eq!(
            bytes_read,
            buf.len().min(BLOCK_SIZE * NUM_BLOCKS - read_offset)
        );
        assert_eq!(
            &buf[..bytes_read],
            &data()
                .skip(read_offset)
                .take(bytes_read)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_read() {
        const DEVICE_BLOCKS: usize = 5;
        const BLOCK_SIZE: usize = 10;

        for offset in 0..BLOCK_SIZE {
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(BLOCK_SIZE / 2, 0 * BLOCK_SIZE + offset);

            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(2 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(3 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(4 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(5 * BLOCK_SIZE, 0 * BLOCK_SIZE + offset);

            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 1 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(2 * BLOCK_SIZE, 1 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(3 * BLOCK_SIZE, 1 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(4 * BLOCK_SIZE, 1 * BLOCK_SIZE + offset);

            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 2 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(2 * BLOCK_SIZE, 2 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(3 * BLOCK_SIZE, 2 * BLOCK_SIZE + offset);

            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 3 * BLOCK_SIZE + offset);
            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(2 * BLOCK_SIZE, 3 * BLOCK_SIZE + offset);

            test_read_blocks::<BLOCK_SIZE, DEVICE_BLOCKS>(1 * BLOCK_SIZE, 4 * BLOCK_SIZE + offset);
        }
    }

    #[test]
    fn test_write_buffer_larger_than_device() {
        const DEVICE_BLOCKS: usize = 5;
        const BLOCK_SIZE: usize = 10;
        const OFFSET: usize = 0;

        let device = TestBlockDevice::<BLOCK_SIZE> {
            id: TestDeviceId(1),
            data: vec![1; DEVICE_BLOCKS * BLOCK_SIZE],
        };

        let mut file = BlockDeviceFile::new(device);

        let buf = vec![0; 100];
        let result = file.write(&buf, OFFSET);
        assert_eq!(result, Err(WriteError::WriteFailed));
    }

    #[test]
    fn test_read_buffer_larger_than_device() {
        const DEVICE_BLOCKS: usize = 5;
        const BLOCK_SIZE: usize = 10;
        const OFFSET: usize = 0;

        let device = TestBlockDevice::<BLOCK_SIZE> {
            id: TestDeviceId(1),
            data: vec![1; DEVICE_BLOCKS * BLOCK_SIZE],
        };

        let mut file = BlockDeviceFile::new(device);

        let mut buf = vec![0; 100];
        let result = file.read(&mut buf, OFFSET);
        assert_eq!(result, Err(ReadError::ReadFailed));
    }

    #[test]
    fn test_write_out_of_bounds() {
        const DEVICE_BLOCKS: usize = 2;
        const BLOCK_SIZE: usize = 10;

        let device = TestBlockDevice::<BLOCK_SIZE> {
            id: TestDeviceId(1),
            data: vec![1; DEVICE_BLOCKS * BLOCK_SIZE],
        };

        let mut file = BlockDeviceFile::new(device);

        let buf = vec![0; 100];
        let result = file.write(&buf, DEVICE_BLOCKS * BLOCK_SIZE);
        assert_eq!(result, Err(WriteError::WriteFailed));
    }

    #[test]
    fn test_read_out_of_bounds() {
        const DEVICE_BLOCKS: usize = 2;
        const BLOCK_SIZE: usize = 10;

        let device = TestBlockDevice::<BLOCK_SIZE> {
            id: TestDeviceId(1),
            data: vec![1; DEVICE_BLOCKS * BLOCK_SIZE],
        };

        let mut file = BlockDeviceFile::new(device);

        let mut buf = vec![0; 100];
        let result = file.read(&mut buf, DEVICE_BLOCKS * BLOCK_SIZE);
        assert_eq!(result, Err(ReadError::ReadFailed));
    }

    #[test]
    fn test_write_empty_buffer() {
        const DEVICE_BLOCKS: usize = 5;
        const BLOCK_SIZE: usize = 10;

        let device = TestBlockDevice::<BLOCK_SIZE> {
            id: TestDeviceId(1),
            data: vec![1; DEVICE_BLOCKS * BLOCK_SIZE],
        };

        let mut file = BlockDeviceFile::new(device);

        let buf: Vec<u8> = vec![];
        let bytes_written = file.write(&buf, 0).unwrap();
        assert_eq!(bytes_written, 0);
    }

    #[test]
    fn test_read_empty_buffer() {
        const DEVICE_BLOCKS: usize = 5;
        const BLOCK_SIZE: usize = 10;

        let device = TestBlockDevice::<BLOCK_SIZE> {
            id: TestDeviceId(1),
            data: vec![1; DEVICE_BLOCKS * BLOCK_SIZE],
        };

        let mut file = BlockDeviceFile::new(device);

        let mut buf: Vec<u8> = vec![];
        let bytes_read = file.read(&mut buf, 0).unwrap();
        assert_eq!(bytes_read, 0);
        assert!(buf.is_empty());
    }
}
