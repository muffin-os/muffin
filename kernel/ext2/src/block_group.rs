use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};

use crate::{
    BlockAddress, bytefield, bytefield_field_read, bytefield_field_write, check_is_implemented,
};

pub const BGD_SIZE: usize = 32;

bytefield! {
    pub struct BlockGroupDescriptor ([u8; BGD_SIZE]) {
        block_usage_bitmap_block: BlockAddress = 0,
        inode_usage_bitmap_block: BlockAddress = 4,
        inode_table_starting_block: BlockAddress = 8,
        num_unallocated_blocks: u16 = 12,
        num_unallocated_inodes: u16 = 14,
        num_directories: u16 = 16,
    }
}

impl BlockGroupDescriptor {
    pub fn block_usage_bitmap_block(&self) -> BlockAddress {
        self.block_usage_bitmap_block
    }

    pub fn inode_usage_bitmap_block(&self) -> BlockAddress {
        self.inode_usage_bitmap_block
    }

    pub fn inode_table_starting_block(&self) -> BlockAddress {
        self.inode_table_starting_block
    }

    pub fn num_unallocated_blocks(&self) -> u16 {
        self.num_unallocated_blocks
    }

    pub fn num_unallocated_blocks_mut(&mut self) -> &mut u16 {
        &mut self.num_unallocated_blocks
    }

    pub fn num_unallocated_inodes(&self) -> u16 {
        self.num_unallocated_inodes
    }

    pub fn num_unallocated_inodes_mut(&mut self) -> &mut u16 {
        &mut self.num_unallocated_inodes
    }

    pub fn num_directories(&self) -> u16 {
        self.num_directories
    }

    pub fn num_directories_mut(&mut self) -> &mut u16 {
        &mut self.num_directories
    }
}

pub type Inner = Vec<BlockGroupDescriptor>;

pub struct BlockGroupDescriptorTable(Inner);

impl BlockGroupDescriptorTable {
    pub fn new() -> Self {
        Self(Inner::new())
    }
}

impl Deref for BlockGroupDescriptorTable {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for BlockGroupDescriptorTable {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
