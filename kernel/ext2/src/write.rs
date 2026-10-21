use alloc::vec;

use kernel_device::block::BlockDevice;

use crate::{BlockAddress, Error, Ext2Fs, IndirectPointerLimits, Inode, RegularFile};

/// Where a file block lives after [`Ext2Fs::map_block`].
enum MappedBlock {
    Existing(BlockAddress),
    /// Freshly allocated, so its on-disk bytes are stale.
    Allocated(BlockAddress),
}

/// One of the three inode pointers that root a tree of indirect tables.
#[derive(Copy, Clone)]
enum Tier {
    Single,
    Double,
    Triple,
}

impl Tier {
    /// The tier holding `block_index`, `None` for the direct blocks.
    fn containing(block_index: u32, limits: &IndirectPointerLimits) -> Option<Self> {
        if block_index < limits.direct_end {
            None
        } else if block_index < limits.indirect_end {
            Some(Self::Single)
        } else if block_index < limits.double_indirect_end {
            Some(Self::Double)
        } else {
            Some(Self::Triple)
        }
    }

    /// First file block index this tier covers.
    fn begin(self, limits: &IndirectPointerLimits) -> u32 {
        match self {
            Self::Single => limits.direct_end,
            Self::Double => limits.indirect_end,
            Self::Triple => limits.double_indirect_end,
        }
    }

    /// Number of tables between the inode and a data block.
    fn depth(self) -> u32 {
        match self {
            Self::Single => 1,
            Self::Double => 2,
            Self::Triple => 3,
        }
    }

    fn root(self, inode: &Inode) -> Option<BlockAddress> {
        match self {
            Self::Single => inode.single_indirect_ptr(),
            Self::Double => inode.double_indirect_ptr(),
            Self::Triple => inode.triple_indirect_ptr(),
        }
    }

    fn set_root(self, inode: &mut Inode, root: Option<BlockAddress>) {
        match self {
            Self::Single => inode.set_single_indirect_ptr(root),
            Self::Double => inode.set_double_indirect_ptr(root),
            Self::Triple => inode.set_triple_indirect_ptr(root),
        }
    }
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

    /// Resolves `block_index` of `inode`, allocating the data block and every
    /// missing table on the way if it is a hole. Table entries are written to
    /// disk; pointers in the inode itself are only set in memory.
    fn map_block(&mut self, inode: &mut Inode, block_index: u32) -> Result<MappedBlock, Error> {
        if let Some(block) = self.resolve_block_index(inode, block_index)? {
            return Ok(MappedBlock::Existing(block));
        }
        let limits = self.indirect_pointer_limits();
        let Some(tier) = Tier::containing(block_index, &limits) else {
            let block = self.allocate_counted(inode)?;
            inode.set_direct_ptr(block_index as usize, Some(block));
            return Ok(MappedBlock::Allocated(block));
        };

        let mut node = match tier.root(inode) {
            Some(table) => table,
            None => {
                let table = self.allocate_table(inode)?;
                tier.set_root(inode, Some(table));
                table
            }
        };
        let relative_index = block_index - tier.begin(&limits);
        let pointers_per_block = self.superblock.pointers_per_block();
        for level in (0..tier.depth()).rev() {
            let slot = relative_index / pointers_per_block.pow(level) % pointers_per_block;
            node = match self.resolve_indirect_ptr(Some(node), slot)? {
                Some(child) => child,
                None => {
                    let child = if level == 0 {
                        self.allocate_counted(inode)?
                    } else {
                        self.allocate_table(inode)?
                    };
                    self.write_table_entry(node, slot, child)?;
                    child
                }
            };
        }
        Ok(MappedBlock::Allocated(node))
    }

    /// Allocates a zeroed indirect table and adds it to the sector count of `inode`.
    fn allocate_table(&mut self, inode: &mut Inode) -> Result<BlockAddress, Error> {
        let table = self.allocate_counted(inode)?;
        self.write_block(table, &vec![0_u8; self.superblock.block_size() as usize])?;
        Ok(table)
    }

    fn write_table_entry(
        &mut self,
        table: BlockAddress,
        slot: u32,
        entry: BlockAddress,
    ) -> Result<(), Error> {
        self.indirect_cache.lock().invalidate(table);
        let offset = self.resolve_block_offset(table) + slot as usize * size_of::<BlockAddress>();
        self.block_device
            .write_at(offset, &entry.into_u32().to_le_bytes())
            .map_err(|_| Error::DeviceWrite)
            .map(|_| ())
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
