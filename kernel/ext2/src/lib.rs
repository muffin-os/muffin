#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use alloc::vec;
use alloc::vec::Vec;

pub use address::*;
pub use dir::*;
pub use error::*;
pub use inode::*;
use kernel_device::block::BlockDevice;
pub use read::IndirectPointerLimits;
use spin::Mutex;
pub use superblock::*;

use crate::block_group::{BGD_SIZE, BlockGroupDescriptor, BlockGroupDescriptorTable};

mod address;
mod block_group;
mod bytefield;
mod create;
mod dir;
mod error;
mod inode;
mod read;
mod superblock;
mod write;

const ROOT_DIR_INODE_ADDRESS: InodeAddress = InodeAddress::new(2).unwrap();

/// An ext2 filesystem over a block device.
///
/// Metadata blocks (bitmaps, descriptors, inode table, indirect tables) live
/// in a write-back cache; file and directory data go straight to the device.
/// Nothing in the cache reaches the device before [`Ext2Fs::sync`].
pub struct Ext2Fs<T> {
    block_device: T,
    superblock: Superblock,
    bgdt: BlockGroupDescriptorTable,
    // shortcut: unbounded, every metadata block touched stays resident; add
    // eviction once a workload touches more metadata than fits in memory
    cache: Mutex<BTreeMap<u32, MetadataBlock>>,
}

struct MetadataBlock {
    data: Vec<u8>,
    dirty: bool,
}

const SUPERBLOCK_OFFSET: usize = 1024;

impl<T> Ext2Fs<T>
where
    T: BlockDevice,
{
    /// Reads and validates the superblock and block group descriptor table.
    ///
    /// Returns [`Error::InvalidSuperblock`] if the magic number is wrong, a
    /// per-group count is zero, or the block size is larger than 64 KiB, and
    /// [`Error::InvalidBlockGroupDescriptor`] if a group points a bitmap or
    /// its inode table at block 0.
    pub fn try_new(block_device: T) -> Result<Self, Error> {
        let mut superblock_data = [0_u8; 1024];
        block_device
            .read_at(SUPERBLOCK_OFFSET, &mut superblock_data)
            .map_err(|_| Error::UnableToReadSuperblock)?;

        let superblock = Superblock::try_from(SuperblockArray::from(superblock_data)).unwrap();
        if superblock.magic_number() != EXT2_MAGIC
            || superblock.blocks_per_group() == 0
            || superblock.inodes_per_group() == 0
            || superblock.log2_block_size() > MAX_LOG2_BLOCK_SIZE
        {
            return Err(Error::InvalidSuperblock);
        }
        let number_of_block_groups = superblock
            .num_blocks()
            .div_ceil(superblock.blocks_per_group());

        let bgdt_offset = superblock.bgdt_offset();

        let block_size = superblock.block_size() as usize;
        let bgdt_len = (number_of_block_groups as usize * BGD_SIZE).next_multiple_of(block_size);
        let mut bgdt_data = vec![0_u8; bgdt_len];
        block_device
            .read_at(bgdt_offset, &mut bgdt_data)
            .map_err(|_| Error::UnableToReadBlockGroupDescriptorTable)?;
        let mut bgdt = BlockGroupDescriptorTable::new();
        for i in 0..number_of_block_groups as usize {
            let offset = i * BGD_SIZE;
            let end = offset + BGD_SIZE;
            let bgd_data = TryInto::<&[u8; BGD_SIZE]>::try_into(&bgdt_data[offset..end]).unwrap();
            let bgd = BlockGroupDescriptor::try_from(bgd_data)
                .map_err(|()| Error::InvalidBlockGroupDescriptor(i as u32))?;
            bgdt.push(bgd);
        }

        Ok(Self {
            block_device,
            superblock,
            bgdt,
            cache: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    pub fn read_root_inode(&self) -> Result<Directory, Error> {
        self.read_inode(ROOT_DIR_INODE_ADDRESS)
            .and_then(|inode| Directory::try_from(inode).map_err(|_| Error::NotDirectory))
    }

    /// Returns a snapshot of the inode. Keep at most one live copy per
    /// address, since [`Ext2Fs::write_inode`] stores a copy whole.
    pub fn read_inode(&self, addr: InodeAddress) -> Result<(InodeAddress, Inode), Error> {
        let (block, within) = self.inode_location(addr)?;
        // inode size can vary, but the specified fields are always between 0 and 128, and we don't need more
        let inode_buffer: [u8; 128] =
            self.read_metadata(block, |data| data[within..within + 128].try_into().unwrap())?;

        Ok((addr, Inode::try_from(InodeRawArray::from(inode_buffer)).expect("inode conversion can't fail. if it does, the logic has changed and this should propagate the error")))
    }

    pub fn write_inode(&mut self, addr: InodeAddress, inode: &Inode) -> Result<(), Error> {
        let (block, within) = self.inode_location(addr)?;
        let inode_raw = InodeRawArray::from(inode);
        self.modify_metadata(block, |data| {
            data[within..within + inode_raw.as_slice().len()].copy_from_slice(inode_raw.as_slice());
        })
    }

    pub fn read_block(&self, addr: BlockAddress, buf: &mut [u8]) -> Result<usize, Error> {
        let offset = self.resolve_block_offset(addr);
        self.block_device
            .read_at(offset, buf)
            .map_err(|_| Error::DeviceRead)
    }

    /// Writes a data block. A cached copy, left over from the block's life as
    /// metadata, is dropped so it never shadows or overwrites the data.
    pub fn write_block(&mut self, addr: BlockAddress, buf: &[u8]) -> Result<usize, Error> {
        self.cache.lock().remove(&addr.get());
        let offset = self.resolve_block_offset(addr);
        self.block_device
            .write_at(offset, buf)
            .map_err(|_| Error::DeviceWrite)
    }

    /// Writes every dirty metadata block to the device. A block whose write
    /// fails stays dirty, so a later call retries it.
    pub fn sync(&mut self) -> Result<(), Error> {
        let block_size = self.superblock.block_size() as usize;
        for (number, block) in self.cache.lock().iter_mut().filter(|(_, b)| b.dirty) {
            self.block_device
                .write_at(*number as usize * block_size, &block.data)
                .map_err(|_| Error::DeviceWrite)?;
            block.dirty = false;
        }
        Ok(())
    }

    fn read_metadata<R>(&self, number: u32, f: impl FnOnce(&[u8]) -> R) -> Result<R, Error> {
        let mut cache = self.cache.lock();
        Ok(f(&self.cached(&mut cache, number)?.data))
    }

    fn modify_metadata<R>(
        &mut self,
        number: u32,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<R, Error> {
        let mut cache = self.cache.lock();
        let block = self.cached(&mut cache, number)?;
        block.dirty = true;
        Ok(f(&mut block.data))
    }

    // a freshly allocated table: its on-disk bytes are stale, so none are read
    pub(crate) fn insert_zeroed_metadata(&mut self, number: u32) {
        let data = vec![0_u8; self.superblock.block_size() as usize];
        self.cache
            .lock()
            .insert(number, MetadataBlock { data, dirty: true });
    }

    // loads the block on a miss
    fn cached<'c>(
        &self,
        cache: &'c mut BTreeMap<u32, MetadataBlock>,
        number: u32,
    ) -> Result<&'c mut MetadataBlock, Error> {
        Ok(match cache.entry(number) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let mut data = vec![0_u8; self.superblock.block_size() as usize];
                self.block_device
                    .read_at(number as usize * data.len(), &mut data)
                    .map_err(|_| Error::DeviceRead)?;
                entry.insert(MetadataBlock { data, dirty: false })
            }
        })
    }

    // block number and byte offset within it
    fn inode_location(&self, addr: InodeAddress) -> Result<(u32, usize), Error> {
        let offset = self.inode_offset(addr)?;
        let block_size = self.superblock.block_size() as usize;
        Ok(((offset / block_size) as u32, offset % block_size))
    }

    fn inode_offset(&self, addr: InodeAddress) -> Result<usize, Error> {
        let inodes_per_group = self.superblock.inodes_per_group();
        let block_group_index = (addr.get() - 1) / inodes_per_group;
        let block_group = self
            .bgdt
            .get(block_group_index as usize)
            .ok_or(Error::InvalidInodeAddress(addr.get()))?;
        let index = (addr.get() - 1) % inodes_per_group;
        Ok(
            self.resolve_block_offset(block_group.inode_table_starting_block())
                + index as usize * usize::from(self.superblock.inode_size()),
        )
    }

    fn resolve_block_offset(&self, addr: BlockAddress) -> usize {
        addr.get() as usize * self.superblock.block_size() as usize
    }

    pub fn allocate_block(&mut self) -> Result<Option<BlockAddress>, Error> {
        self.allocate(Resource::Block)
            .map(|block| block.and_then(BlockAddress::new))
    }

    pub fn allocate_inode(&mut self) -> Result<Option<InodeAddress>, Error> {
        self.allocate(Resource::Inode)
            .map(|inode| inode.and_then(InodeAddress::new))
    }

    /// Marks `block` free in its group's bitmap and counts it in the group and
    /// superblock free counters.
    ///
    /// # Errors
    /// - [`Error::InvalidBlockAddress`] if `block` lies outside the data
    ///   blocks or is not marked allocated; nothing changes.
    /// - [`Error::DeviceRead`] if a metadata block cannot be loaded.
    pub fn deallocate_block(&mut self, block: BlockAddress) -> Result<(), Error> {
        self.deallocate(Resource::Block, block.get())
    }

    fn deallocate(&mut self, resource: Resource, number: u32) -> Result<(), Error> {
        let (per_group, first_number, bit_count, invalid) = match resource {
            Resource::Block => (
                self.superblock.blocks_per_group(),
                self.superblock.first_data_block(),
                self.superblock
                    .num_blocks()
                    .saturating_sub(self.superblock.first_data_block()),
                Error::InvalidBlockAddress(number),
            ),
            Resource::Inode => (
                self.superblock.inodes_per_group(),
                1,
                self.superblock.num_inodes(),
                Error::InvalidInodeAddress(number),
            ),
        };
        let bit_number = number
            .checked_sub(first_number)
            .filter(|&bit| bit < bit_count)
            .ok_or(invalid)?;
        let group_index = (bit_number / per_group) as usize;
        let bit = (bit_number % per_group) as usize;
        let descriptor = self.bgdt.get(group_index).ok_or(invalid)?;
        let bitmap_block = match resource {
            Resource::Block => descriptor.block_usage_bitmap_block(),
            Resource::Inode => descriptor.inode_usage_bitmap_block(),
        };

        let mask = 1 << (bit % 8);
        let byte_index = bit / 8;
        let is_allocated = self.read_metadata(bitmap_block.get(), |bitmap| {
            bitmap.get(byte_index).is_some_and(|byte| byte & mask != 0)
        })?;
        if !is_allocated {
            return Err(invalid);
        }
        self.modify_metadata(bitmap_block.get(), |bitmap| bitmap[byte_index] &= !mask)?;
        // a freed metadata block must not resurface with stale contents
        if resource == Resource::Block {
            self.cache.lock().remove(&number);
        }

        let descriptor = &mut self.bgdt[group_index];
        match resource {
            Resource::Block => {
                let free = descriptor.num_unallocated_blocks_mut();
                *free = free.saturating_add(1);
                let free = self.superblock.num_unallocated_blocks_mut();
                *free = free.saturating_add(1);
            }
            Resource::Inode => {
                let free = descriptor.num_unallocated_inodes_mut();
                *free = free.saturating_add(1);
                let free = self.superblock.num_unallocated_inodes_mut();
                *free = free.saturating_add(1);
            }
        }
        self.persist_counters(group_index)
    }

    fn allocate(&mut self, resource: Resource) -> Result<Option<u32>, Error> {
        // bitmap bit 0 is inode 1 resp. block s_first_data_block
        let (per_group, first_number) = match resource {
            Resource::Block => (
                self.superblock.blocks_per_group(),
                self.superblock.first_data_block(),
            ),
            Resource::Inode => (self.superblock.inodes_per_group(), 1),
        };

        for group_index in 0..self.bgdt.len() {
            let descriptor = &self.bgdt[group_index];
            let bitmap_block = match resource {
                Resource::Block => descriptor.block_usage_bitmap_block(),
                Resource::Inode => descriptor.inode_usage_bitmap_block(),
            };
            let Some(bit) = self.reserve_first_free_bit(bitmap_block)? else {
                continue;
            };

            let descriptor = &mut self.bgdt[group_index];
            let (group_free, superblock_free) = match resource {
                Resource::Block => (
                    descriptor.num_unallocated_blocks_mut(),
                    self.superblock.num_unallocated_blocks_mut(),
                ),
                Resource::Inode => (
                    descriptor.num_unallocated_inodes_mut(),
                    self.superblock.num_unallocated_inodes_mut(),
                ),
            };
            // the bitmap is the truth, a stale counter must not panic
            *group_free = group_free.saturating_sub(1);
            *superblock_free = superblock_free.saturating_sub(1);
            self.persist_counters(group_index)?;
            return Ok(Some(group_index as u32 * per_group + bit + first_number));
        }

        Ok(None)
    }

    // the group's counters and the superblock's change together
    fn persist_counters(&mut self, group_index: usize) -> Result<(), Error> {
        // the structs model a subset of the on-disk fields, so writing them
        // whole would zero the rest
        const BGD_FREE_COUNTS_OFFSET: usize = 12;
        const SUPERBLOCK_FREE_COUNTS_OFFSET: usize = 12;
        let block_size = self.superblock.block_size() as usize;

        let descriptor = &self.bgdt[group_index];
        let bgd_counts = [
            descriptor.num_unallocated_blocks().to_le_bytes(),
            descriptor.num_unallocated_inodes().to_le_bytes(),
            descriptor.num_directories().to_le_bytes(),
        ]
        .concat();
        let offset =
            self.superblock.bgdt_offset() + group_index * BGD_SIZE + BGD_FREE_COUNTS_OFFSET;
        self.modify_metadata((offset / block_size) as u32, |data| {
            data[offset % block_size..][..bgd_counts.len()].copy_from_slice(&bgd_counts);
        })?;

        let superblock_counts = [
            self.superblock.num_unallocated_blocks().to_le_bytes(),
            self.superblock.num_unallocated_inodes().to_le_bytes(),
        ]
        .concat();
        let offset = SUPERBLOCK_OFFSET + SUPERBLOCK_FREE_COUNTS_OFFSET;
        self.modify_metadata((offset / block_size) as u32, |data| {
            data[offset % block_size..][..superblock_counts.len()]
                .copy_from_slice(&superblock_counts);
        })
    }

    fn reserve_first_free_bit(&mut self, bitmap_block: BlockAddress) -> Result<Option<u32>, Error> {
        let Some(i) = self.read_metadata(bitmap_block.get(), |bitmap| {
            bitmap.iter().position(|byte| *byte != u8::MAX)
        })?
        else {
            return Ok(None);
        };
        self.modify_metadata(bitmap_block.get(), |bitmap| {
            let bit_index = bitmap[i].trailing_ones();
            bitmap[i] |= 1 << bit_index;
            Some(i as u32 * 8 + bit_index)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Resource {
    Block,
    Inode,
}
