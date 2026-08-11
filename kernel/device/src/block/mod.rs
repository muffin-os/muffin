use alloc::sync::Arc;
use alloc::vec;

pub use mem::*;
use spin::RwLock;

mod mem;

pub trait BlockDevice {
    type Error;

    /// Determines the sector size of this device.
    /// The returned value must never change.
    fn sector_size(&self) -> usize;

    /// Determines the amount of sectors that
    /// this device has available.
    fn sector_count(&self) -> usize;

    /// Reads the sector with the given sector_index into the given buffer.
    /// The buffer must be exactly as big as the value returned by
    /// [`BlockDevice::sector_size`], otherwise an implementation may
    /// panic.
    /// Reads that are out of the bounds of this device must not panic, but return
    /// an appropriate error.
    fn read_sector(&self, sector_index: usize, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Writes the given buffer into the sector with the given sector
    /// index. The buffer must be exactly as big as the value returned
    /// by [`BlockDevice::sector_size`], otherwise an implementation may
    /// panic.
    /// Reads that are out of the bounds of this device must not panic, but return
    /// an appropriate error.
    fn write_sector(&mut self, sector_index: usize, buf: &[u8]) -> Result<usize, Self::Error>;

    /// Reads `buf.len()` bytes starting at the given **byte** offset
    /// into the given buffer. Returns an error if the read would exceed
    /// the length of this block device.
    ///
    /// Zero-sized reads are allowed.
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        let sector_size = self.sector_size();
        if offset.is_multiple_of(sector_size) && buf.len() == sector_size {
            return self.read_sector(offset / sector_size, buf);
        }

        let start_sector = offset / sector_size;
        let relative_offset = offset % sector_size;
        let end_sector = (offset + buf.len() - 1) / sector_size;

        let mut data = vec![0_u8; (end_sector - start_sector + 1) * sector_size];
        for (index, sector) in data.chunks_exact_mut(sector_size).enumerate() {
            self.read_sector(start_sector + index, sector)?;
        }
        buf.copy_from_slice(&data[relative_offset..relative_offset + buf.len()]);

        Ok(buf.len())
    }

    /// Writes `buf.len()` bytes starting at the given **byte** offset
    /// onto the block device. Returns an error if the write would exceed
    /// the length of this block device.
    ///
    /// Zero-sized writes are allowed.
    fn write_at(&mut self, offset: usize, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        let sector_size = self.sector_size();
        if offset.is_multiple_of(sector_size) && buf.len() == sector_size {
            return self.write_sector(offset / sector_size, buf);
        }

        let start_sector = offset / sector_size;
        let start_offset = offset % sector_size;
        let end_sector = (offset + buf.len() - 1) / sector_size;
        let end_offset = offset + buf.len() - end_sector * sector_size;

        if start_sector == end_sector {
            let mut sector = vec![0_u8; sector_size];
            self.read_sector(start_sector, &mut sector)?;
            sector[start_offset..end_offset].copy_from_slice(buf);
            self.write_sector(start_sector, &sector)?;
            return Ok(buf.len());
        }

        let head_len = sector_size - start_offset;
        if start_offset == 0 {
            self.write_sector(start_sector, &buf[..sector_size])?;
        } else {
            let mut sector = vec![0_u8; sector_size];
            self.read_sector(start_sector, &mut sector)?;
            sector[start_offset..].copy_from_slice(&buf[..head_len]);
            self.write_sector(start_sector, &sector)?;
        }

        let tail_start = buf.len() - end_offset;
        for (index, chunk) in buf[head_len..tail_start]
            .chunks_exact(sector_size)
            .enumerate()
        {
            self.write_sector(start_sector + index + 1, chunk)?;
        }

        if end_offset == sector_size {
            self.write_sector(end_sector, &buf[tail_start..])?;
        } else {
            let mut sector = vec![0_u8; sector_size];
            self.read_sector(end_sector, &mut sector)?;
            sector[..end_offset].copy_from_slice(&buf[tail_start..]);
            self.write_sector(end_sector, &sector)?;
        }

        Ok(buf.len())
    }
}

impl<T> BlockDevice for Arc<RwLock<T>>
where
    T: BlockDevice + ?Sized,
{
    type Error = T::Error;

    fn sector_size(&self) -> usize {
        self.read().sector_size()
    }

    fn sector_count(&self) -> usize {
        self.read().sector_count()
    }

    fn read_sector(&self, sector_index: usize, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.read().read_sector(sector_index, buf)
    }

    fn write_sector(&mut self, sector_index: usize, buf: &[u8]) -> Result<usize, Self::Error> {
        self.write().write_sector(sector_index, buf)
    }
}
