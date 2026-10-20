#![no_std]

extern crate alloc;

use alloc::vec;

pub use address::*;
pub use dir::*;
pub use error::*;
pub use inode::*;
use kernel_device::block::BlockDevice;
pub use read::IndirectPointerLimits;
use spin::Mutex;
pub use superblock::*;

use crate::block_group::{BGD_SIZE, BlockGroupDescriptor, BlockGroupDescriptorTable};
use crate::read::IndirectCache;

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
pub struct Ext2Fs<T> {
    block_device: T,
    superblock: Superblock,
    bgdt: BlockGroupDescriptorTable,
    indirect_cache: Mutex<IndirectCache>,
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
            indirect_cache: Mutex::new(IndirectCache::new()),
        })
    }

    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    pub fn read_root_inode(&self) -> Result<Directory, Error> {
        self.read_inode(ROOT_DIR_INODE_ADDRESS)
            .and_then(|inode| Directory::try_from(inode).map_err(|_| Error::NotDirectory))
    }

    pub fn read_inode(&self, addr: InodeAddress) -> Result<(InodeAddress, Inode), Error> {
        // shortcut: every call returns a fresh copy, so two live copies of one
        // inode overwrite each other's writes. Centralize inodes in a cache
        // before anything holds two handles to the same file.
        let inodes_per_group = self.superblock.inodes_per_group();
        let block_group_index = (addr.get() - 1) / inodes_per_group;
        let block_group = &self.bgdt[block_group_index as usize];
        let itable_start_block = block_group.inode_table_starting_block();

        let index = (addr.get() - 1) % inodes_per_group;
        let inode_size = self.superblock.inode_size();
        let address =
            self.resolve_block_offset(itable_start_block) + (index * inode_size as u32) as usize;

        let mut inode_buffer = [0_u8; 128]; // inode size can vary, but the specified fields are always between 0 and 128, and we don't need more
        self.block_device
            .read_at(address, &mut inode_buffer)
            .map_err(|_| Error::DeviceRead)?;

        Ok((addr, Inode::try_from(InodeRawArray::from(inode_buffer)).expect("inode conversion can't fail. if it does, the logic has changed and this should propagate the error")))
    }

    pub fn write_inode(&mut self, addr: InodeAddress, inode: &Inode) -> Result<(), Error> {
        let inodes_per_group = self.superblock.inodes_per_group();
        let block_group_index = (addr.get() - 1) / inodes_per_group;
        let block_group = &self.bgdt[block_group_index as usize];
        let itable_start_block = block_group.inode_table_starting_block();

        let index = (addr.get() - 1) % inodes_per_group;
        let inode_size = self.superblock.inode_size();
        let address =
            self.resolve_block_offset(itable_start_block) + (index * inode_size as u32) as usize;

        let inode_raw = InodeRawArray::from(inode);
        self.block_device
            .write_at(address, inode_raw.as_slice())
            .map_err(|_| Error::DeviceWrite)
            .map(|_| ())
    }

    pub fn read_block(&self, addr: BlockAddress, buf: &mut [u8]) -> Result<usize, Error> {
        let offset = self.resolve_block_offset(addr);
        self.block_device
            .read_at(offset, buf)
            .map_err(|_| Error::DeviceRead)
    }

    pub fn write_block(&mut self, addr: BlockAddress, buf: &[u8]) -> Result<usize, Error> {
        self.indirect_cache.lock().invalidate(addr);
        let offset = self.resolve_block_offset(addr);
        self.block_device
            .write_at(offset, buf)
            .map_err(|_| Error::DeviceWrite)
    }

    fn resolve_block_offset(&self, addr: BlockAddress) -> usize {
        (addr.get() * self.superblock.block_size()) as usize
    }

    pub fn allocate_block(&mut self) -> Result<Option<BlockAddress>, Error> {
        self.allocate(Resource::Block)
            .map(|block| block.and_then(BlockAddress::new))
    }

    pub fn allocate_inode(&mut self) -> Result<Option<InodeAddress>, Error> {
        self.allocate(Resource::Inode)
            .map(|inode| inode.and_then(InodeAddress::new))
    }

    fn allocate(&mut self, resource: Resource) -> Result<Option<u32>, Error> {
        let block_size = self.superblock.block_size();
        let bgdt_offset = self.superblock.bgdt_offset();
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
            match resource {
                Resource::Block => {
                    *descriptor.num_unallocated_blocks_mut() -= 1;
                    *self.superblock.num_unallocated_blocks_mut() -= 1;
                }
                Resource::Inode => {
                    *descriptor.num_unallocated_inodes_mut() -= 1;
                    *self.superblock.num_unallocated_inodes_mut() -= 1;
                }
            }

            let mut bgdt_data = vec![0_u8; block_size as usize];
            self.block_device
                .read_at(bgdt_offset, &mut bgdt_data)
                .map_err(|_| Error::UnableToReadBlockGroupDescriptorTable)?;
            let bgd_offset = group_index * BGD_SIZE;
            let bgd_end = bgd_offset + BGD_SIZE;
            let bgd_data = Into::<[u8; BGD_SIZE]>::into(&*descriptor);
            bgdt_data[bgd_offset..bgd_end].copy_from_slice(&bgd_data);
            self.block_device
                .write_at(bgdt_offset, &bgdt_data)
                .map_err(|_| Error::UnableToWriteBlockGroupDescriptorTable)?;

            let superblock_data = Into::<SuperblockArray>::into(&self.superblock);
            self.block_device
                .write_at(SUPERBLOCK_OFFSET, superblock_data.as_slice())
                .map_err(|_| Error::UnableToWriteSuperblock)?;

            return Ok(Some(group_index as u32 * per_group + bit + first_number));
        }

        Ok(None)
    }

    fn reserve_first_free_bit(&mut self, bitmap_block: BlockAddress) -> Result<Option<u32>, Error> {
        let mut bitmap = vec![0_u8; self.superblock.block_size() as usize];
        self.read_block(bitmap_block, &mut bitmap)?;

        for (i, byte) in bitmap.iter_mut().enumerate() {
            if *byte == u8::MAX {
                continue;
            }
            let bit_index = byte.trailing_ones();
            *byte |= 1 << bit_index;
            self.write_block(bitmap_block, &bitmap)?;
            return Ok(Some(i as u32 * 8 + bit_index));
        }
        Ok(None)
    }
}

#[derive(Clone, Copy)]
enum Resource {
    Block,
    Inode,
}
