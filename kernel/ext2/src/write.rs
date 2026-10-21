use alloc::vec;

use kernel_device::block::BlockDevice;

use crate::{BlockAddress, Error, Ext2Fs, IndirectPointerLimits, Inode, RegularFile};

enum MappedBlock {
    Existing(BlockAddress),
    // freshly allocated, so its on-disk bytes are stale
    Allocated(BlockAddress),
}

// the three inode pointers that each root a tree of indirect tables
#[derive(Copy, Clone)]
enum Tier {
    Single,
    Double,
    Triple,
}

impl Tier {
    // None for the direct blocks
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

    // first file block index the tier covers
    fn begin(self, limits: &IndirectPointerLimits) -> u32 {
        match self {
            Self::Single => limits.direct_end,
            Self::Double => limits.indirect_end,
            Self::Triple => limits.double_indirect_end,
        }
    }

    // tables between the inode and a data block
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
    /// The inode reflects every block allocated, also when an error stops
    /// the write early. In that case the bytes written so far are returned,
    /// or the error if nothing was written.
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

    // table entries go to disk here, the inode's own pointers only change in memory
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

    fn allocate_table(&mut self, inode: &mut Inode) -> Result<BlockAddress, Error> {
        let table = self.allocate_counted(inode)?;
        self.insert_zeroed_metadata(table.get());
        Ok(table)
    }

    fn write_table_entry(
        &mut self,
        table: BlockAddress,
        slot: u32,
        entry: BlockAddress,
    ) -> Result<(), Error> {
        let at = slot as usize * size_of::<BlockAddress>();
        self.modify_metadata(table.get(), |data| {
            data[at..at + size_of::<BlockAddress>()]
                .copy_from_slice(&entry.into_u32().to_le_bytes());
        })
    }

    // the sector count is checked first, so an overflow never leaks the block
    pub(crate) fn allocate_counted(&mut self, inode: &mut Inode) -> Result<BlockAddress, Error> {
        let sectors = inode
            .num_disk_sectors()
            .checked_add(self.superblock.sectors_per_block())
            .ok_or(Error::FileTooLarge)?;
        let block = self.allocate_block()?.ok_or(Error::NoSpace)?;
        *inode.num_disk_sectors_mut() = sectors;
        Ok(block)
    }

    /// Sets the byte size of `file` to `len_bytes`. Shrinking frees every data
    /// block past the new end and every indirect table left empty, and zeroes
    /// the rest of the block holding the new end, so growing later reads
    /// zeros. Growing allocates nothing; the new range is a hole.
    ///
    /// Blocks freed before an error stay freed and unlinked on disk, and the
    /// size is left unchanged.
    ///
    /// # Errors
    /// - [`Error::FileTooLarge`] if `len_bytes` lies past the last addressable
    ///   block; nothing changes.
    /// - [`Error::InvalidBlockAddress`] if the file points at a block that is
    ///   not marked allocated.
    /// - [`Error::DeviceRead`] / [`Error::DeviceWrite`] and the deallocator's
    ///   superblock and descriptor table errors if the device fails.
    pub fn truncate_file(&mut self, file: &mut RegularFile, len_bytes: usize) -> Result<(), Error> {
        if len_bytes == file.len() {
            return Ok(());
        }
        if let Some(last_byte) = len_bytes.checked_sub(1) {
            self.file_block_index(last_byte)?;
        }
        if len_bytes > file.len() {
            file.inode_mut().set_len(len_bytes);
            return self.write_inode(file.inode_address(), file);
        }

        let block_size = self.superblock.block_size() as usize;
        let tail_begin = len_bytes % block_size;
        if tail_begin != 0
            && let Some(block) =
                self.resolve_block_index(file, self.file_block_index(len_bytes)?)?
        {
            let mut data = vec![0_u8; block_size];
            self.read_block(block, &mut data)?;
            data[tail_begin..].fill(0);
            self.write_block(block, &data)?;
        }

        let kept_blocks = len_bytes.div_ceil(block_size) as u64;
        let outcome = self.free_blocks_from(file.inode_mut(), kept_blocks);
        if outcome.is_ok() {
            file.inode_mut().set_len(len_bytes);
        }
        self.write_inode(file.inode_address(), file)?;
        outcome
    }

    // the inode's own pointers only change in memory
    fn free_blocks_from(&mut self, inode: &mut Inode, kept_blocks: u64) -> Result<(), Error> {
        let limits = self.indirect_pointer_limits();
        for slot in 0..limits.direct_end as usize {
            if (slot as u64) < kept_blocks {
                continue;
            }
            let direct = inode.direct_ptrs().nth(slot).flatten();
            if let Some(block) = direct {
                self.free_counted(inode, block)?;
                inode.set_direct_ptr(slot, None);
            }
        }

        let pointers_per_block = u64::from(self.superblock.pointers_per_block());
        for tier in [Tier::Single, Tier::Double, Tier::Triple] {
            let Some(root) = tier.root(inode) else {
                continue;
            };
            let kept_in_tier = kept_blocks.saturating_sub(u64::from(tier.begin(&limits)));
            if kept_in_tier >= pointers_per_block.pow(tier.depth()) {
                continue;
            }
            if self.free_table_from(inode, root, tier.depth(), kept_in_tier)? {
                self.free_counted(inode, root)?;
                tier.set_root(inode, None);
            }
        }
        Ok(())
    }

    // returns whether `table` is left empty; the caller frees it
    fn free_table_from(
        &mut self,
        inode: &mut Inode,
        table: BlockAddress,
        depth: u32,
        kept: u64,
    ) -> Result<bool, Error> {
        let mut entries = self.read_metadata(table.get(), <[u8]>::to_vec)?;
        let child_span = u64::from(self.superblock.pointers_per_block()).pow(depth - 1);

        let mut is_empty = true;
        let mut is_changed = false;
        let mut outcome = Ok(());
        for (slot, entry) in (0_u64..).zip(entries.as_chunks_mut::<4>().0) {
            let Some(child) = BlockAddress::new(u32::from_le_bytes(*entry)) else {
                continue;
            };
            let child_begin = slot * child_span;
            let freed = if child_begin + child_span <= kept {
                Ok(false)
            } else if depth == 1 {
                Ok(true)
            } else {
                self.free_table_from(inode, child, depth - 1, kept.saturating_sub(child_begin))
            }
            .and_then(|is_freed| {
                if is_freed {
                    self.free_counted(inode, child)?;
                }
                Ok(is_freed)
            });
            match freed {
                Ok(true) => {
                    *entry = [0; 4];
                    is_changed = true;
                }
                Ok(false) => is_empty = false,
                Err(e) => {
                    outcome = Err(e);
                    break;
                }
            }
        }
        // entries of freed children must not survive an error further down
        if is_changed {
            self.modify_metadata(table.get(), |data| data.copy_from_slice(&entries))?;
        }
        outcome.map(|()| is_empty)
    }

    fn free_counted(&mut self, inode: &mut Inode, block: BlockAddress) -> Result<(), Error> {
        self.deallocate_block(block)?;
        let sectors = inode.num_disk_sectors_mut();
        *sectors = sectors.saturating_sub(self.superblock.sectors_per_block());
        Ok(())
    }
}
