use alloc::vec;

use kernel_device::block::BlockDevice;

use crate::{BlockAddress, Error, Ext2Fs, Inode, RegularFile};

/// Where a file block lives after [`Ext2Fs::map_block`].
enum MappedBlock {
    Existing(BlockAddress),
    /// Freshly allocated, so its on-disk bytes are stale.
    Allocated(BlockAddress),
}

impl<T> Ext2Fs<T>
where
    T: BlockDevice,
{
    /// Writes `buf` at byte `offset` of `file`, allocating every missing block
    /// in the range, and grows the file if the write ends past its size.
    ///
    /// The inode on disk reflects every block allocated, also when an error
    /// stops the write early. In that case the bytes written so far are
    /// returned, or the error if nothing was written.
    ///
    /// # Errors
    /// - [`Error::FileTooLarge`] if the write would end past the last
    ///   addressable block; nothing is written.
    /// - [`Error::NoSpace`] if no block was free for the first block written.
    /// - [`Error::DeviceRead`] / [`Error::DeviceWrite`] and the allocator's
    ///   superblock and descriptor table errors if the device fails.
    pub fn write_to_file(
        &mut self,
        file: &mut RegularFile,
        offset: usize,
        buf: &[u8],
    ) -> Result<usize, Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        let end = offset.checked_add(buf.len()).ok_or(Error::FileTooLarge)?;
        let first_block_index = self.file_block_index(offset)?;
        let last_block_index = self.file_block_index(end - 1)?;
        let block_size = self.superblock.block_size() as usize;

        let mut scratch = vec![0_u8; block_size];
        let mut written = 0;
        let mut failure = None;
        for block_index in first_block_index..=last_block_index {
            let within_block = (offset + written) % block_size;
            let chunk_len = (block_size - within_block).min(buf.len() - written);
            let chunk = &buf[written..written + chunk_len];
            let is_partial = chunk_len < block_size;
            let result = self
                .map_block(file.inode_mut(), block_index)
                .and_then(|mapped| {
                    let block = match mapped {
                        MappedBlock::Existing(block) if is_partial => {
                            self.read_block(block, &mut scratch)?;
                            block
                        }
                        MappedBlock::Allocated(block) if is_partial => {
                            scratch.fill(0);
                            block
                        }
                        MappedBlock::Existing(block) | MappedBlock::Allocated(block) => {
                            return self.write_block(block, chunk);
                        }
                    };
                    scratch[within_block..within_block + chunk_len].copy_from_slice(chunk);
                    self.write_block(block, &scratch)
                });
            if let Err(e) = result {
                failure = Some(e);
                break;
            }
            written += chunk_len;
        }

        if offset + written > file.len() {
            file.inode_mut().set_len(offset + written);
        }
        self.write_inode(file.inode_address(), file)?;
        match failure {
            Some(e) if written == 0 => Err(e),
            Some(_) | None => Ok(written),
        }
    }

    /// File block index that holds byte `offset`.
    fn file_block_index(&self, offset: usize) -> Result<u32, Error> {
        u32::try_from(offset / self.superblock.block_size() as usize)
            .ok()
            .filter(|&index| u64::from(index) < self.triple_indirect_end())
            .ok_or(Error::FileTooLarge)
    }

    /// Resolves `block_index` of `inode`, allocating the data block if it is a
    /// hole. Allocations are recorded in the in-memory `inode` only.
    fn map_block(&mut self, inode: &mut Inode, block_index: u32) -> Result<MappedBlock, Error> {
        if let Some(block) = self.resolve_block_index(inode, block_index)? {
            return Ok(MappedBlock::Existing(block));
        }
        let Ok(slot) = usize::try_from(block_index) else {
            return Err(Error::InvalidBlockIndex(block_index));
        };
        if slot >= 12 {
            todo!("allocate indirect block");
        }
        let block = self.allocate_counted(inode)?;
        inode.set_direct_ptr(slot, Some(block));
        Ok(MappedBlock::Allocated(block))
    }

    /// Allocates a block and adds it to the sector count of `inode`.
    fn allocate_counted(&mut self, inode: &mut Inode) -> Result<BlockAddress, Error> {
        let sectors = inode
            .num_disk_sectors()
            .checked_add(self.superblock.sectors_per_block())
            .ok_or(Error::FileTooLarge)?;
        let block = self.allocate_block()?.ok_or(Error::NoSpace)?;
        *inode.num_disk_sectors_mut() = sectors;
        Ok(block)
    }
}
