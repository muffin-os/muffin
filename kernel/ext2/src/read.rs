use alloc::vec;
use alloc::vec::Vec;

use kernel_device::block::BlockDevice;

use crate::{BlockAddress, Error, Ext2Fs, Inode, RegularFile};

const SZ: usize = size_of::<BlockAddress>();
// A triple indirect resolution touches three distinct tables, so four slots
// let a sequential scan keep every table it is about to need again.
const INDIRECT_CACHE_CAPACITY: usize = 4;

/// Exclusive upper bound of the file block index each pointer tier covers.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct IndirectPointerLimits {
    pub direct_end: u32,
    pub indirect_end: u32,
    pub double_indirect_end: u32,
}

/// Most recently used indirect block tables, newest first.
pub(crate) struct IndirectCache {
    entries: Vec<(BlockAddress, Vec<Option<BlockAddress>>)>,
}

impl IndirectCache {
    pub(crate) fn new() -> Self {
        Self { entries: vec![] }
    }

    fn lookup(&mut self, addr: BlockAddress, index: usize) -> Option<Option<BlockAddress>> {
        let pos = self.entries.iter().position(|(a, _)| *a == addr)?;
        self.entries[..=pos].rotate_right(1);
        self.entries[0].1.get(index).copied()
    }

    fn insert(&mut self, addr: BlockAddress, table: Vec<Option<BlockAddress>>) {
        if self.entries.len() == INDIRECT_CACHE_CAPACITY {
            self.entries.pop();
        }
        self.entries.insert(0, (addr, table));
    }

    pub(crate) fn invalidate(&mut self, addr: BlockAddress) {
        self.entries.retain(|(a, _)| *a != addr);
    }
}

impl<T> Ext2Fs<T>
where
    T: BlockDevice,
{
    pub fn read_from_file(
        &self,
        file: &RegularFile,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<usize, Error> {
        let file_size = file.len();
        if offset >= file_size || buf.is_empty() {
            return Ok(0);
        }
        let len = buf.len().min(file_size - offset);
        let start_block = self.file_block_index(offset)?;
        let last_block = self.file_block_index(offset + len - 1)?;

        let block_size = self.superblock.block_size() as usize;
        let relative_offset = offset % block_size;
        let block_count = (last_block - start_block) as usize + 1;
        let mut data: Vec<u8> = vec![0_u8; block_count * block_size];
        let res = self.read_blocks_from_inode(file, start_block, last_block, &mut data)?;
        let total_read = res.min(len);
        buf[..total_read].copy_from_slice(&data[relative_offset..relative_offset + total_read]);

        Ok(total_read)
    }

    // FileTooLarge past the last addressable block
    pub(crate) fn file_block_index(&self, offset: usize) -> Result<u32, Error> {
        u32::try_from(offset / self.superblock.block_size() as usize)
            .ok()
            .filter(|&index| u64::from(index) < self.triple_indirect_end())
            .ok_or(Error::FileTooLarge)
    }

    fn read_blocks_from_inode(
        &self,
        inode: &Inode,
        start_block: u32,
        last_block: u32,
        buf: &mut [u8],
    ) -> Result<usize, Error> {
        let block_size = self.superblock.block_size() as usize;
        let block_count = (last_block - start_block) as usize + 1;
        assert_eq!(
            buf.len(),
            block_count * block_size,
            "buf.len() must be equal to the number of blocks you want to read"
        );

        let mut pointers = Vec::with_capacity(block_count);
        for block in start_block..=last_block {
            pointers.push(self.resolve_block_index(inode, block)?);
        }

        let mut total_read = 0;
        let mut start = 0;
        while start < pointers.len() {
            let mut end = start + 1;
            match pointers[start] {
                None => {
                    while end < pointers.len() && pointers[end].is_none() {
                        end += 1;
                    }
                    buf[start * block_size..end * block_size].fill(0);
                    total_read += (end - start) * block_size;
                }
                Some(first) => {
                    let mut previous = first;
                    while end < pointers.len()
                        && let Some(next) = pointers[end]
                        && previous.get().checked_add(1) == Some(next.get())
                    {
                        previous = next;
                        end += 1;
                    }
                    total_read += self
                        .block_device
                        .read_at(
                            self.resolve_block_offset(first),
                            &mut buf[start * block_size..end * block_size],
                        )
                        .map_err(|_| Error::DeviceRead)?;
                }
            }
            start = end;
        }

        Ok(total_read)
    }

    pub fn indirect_pointer_limits(&self) -> IndirectPointerLimits {
        let pointers_per_block = self.superblock.pointers_per_block();
        let direct_end = 12;
        let indirect_end = direct_end + pointers_per_block;
        IndirectPointerLimits {
            direct_end,
            indirect_end,
            double_indirect_end: indirect_end + pointers_per_block * pointers_per_block,
        }
    }

    // exclusive upper bound of the file block index the triple indirect tier covers
    pub(crate) fn triple_indirect_end(&self) -> u64 {
        let pointers_per_block = u64::from(self.superblock.pointers_per_block());
        u64::from(self.indirect_pointer_limits().double_indirect_end) + pointers_per_block.pow(3)
    }

    pub fn is_block_allocated(&self, inode: &Inode, block_index: u32) -> Result<bool, Error> {
        self.resolve_block_index(inode, block_index)
            .map(|block| block.is_some())
    }

    pub fn resolve_block_index(
        &self,
        inode: &Inode,
        block_index: u32,
    ) -> Result<Option<BlockAddress>, Error> {
        let IndirectPointerLimits {
            direct_end,
            indirect_end,
            double_indirect_end,
        } = self.indirect_pointer_limits();

        Ok(if block_index < direct_end {
            inode.direct_ptrs().nth(block_index as usize).flatten()
        } else if block_index < indirect_end {
            self.resolve_indirect_ptr(inode.single_indirect_ptr(), block_index - direct_end)?
        } else if block_index < double_indirect_end {
            self.resolve_double_indirect_ptr(
                inode.double_indirect_ptr(),
                block_index - indirect_end,
            )?
        } else {
            if u64::from(block_index) >= self.triple_indirect_end() {
                return Err(Error::InvalidBlockIndex(block_index));
            }
            self.resolve_triple_indirect_ptr(
                inode.triple_indirect_ptr(),
                block_index - double_indirect_end,
            )?
        })
    }

    pub fn resolve_indirect_ptr(
        &self,
        indirect_ptr: Option<BlockAddress>,
        block_index: u32,
    ) -> Result<Option<BlockAddress>, Error> {
        let Some(indirect_ptr) = indirect_ptr else {
            return Ok(None);
        };
        let index = block_index as usize;

        if let Some(cached) = self.indirect_cache.lock().lookup(indirect_ptr, index) {
            return Ok(cached);
        }

        let mut indirect_block_data = vec![0_u8; self.superblock.block_size() as usize];
        self.read_block(indirect_ptr, &mut indirect_block_data)?;
        let (chunks, _) = indirect_block_data.as_chunks::<SZ>();
        let table = chunks
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .map(BlockAddress::new)
            .collect::<Vec<_>>();

        let resolved = table
            .get(index)
            .copied()
            .ok_or(Error::InvalidBlockIndex(block_index))?;
        self.indirect_cache.lock().insert(indirect_ptr, table);
        Ok(resolved)
    }

    pub fn resolve_double_indirect_ptr(
        &self,
        double_indirect_block: Option<BlockAddress>,
        block_index: u32,
    ) -> Result<Option<BlockAddress>, Error> {
        let single_indirect_block_size = self.superblock.pointers_per_block();
        let single_indirect_index = block_index / single_indirect_block_size;

        self.resolve_indirect_ptr(double_indirect_block, single_indirect_index)
            .and_then(|single_indirect_block_ptr| {
                self.resolve_indirect_ptr(
                    single_indirect_block_ptr,
                    block_index % single_indirect_block_size,
                )
            })
    }

    pub fn resolve_triple_indirect_ptr(
        &self,
        triple_indirect_block: Option<BlockAddress>,
        block_index: u32,
    ) -> Result<Option<BlockAddress>, Error> {
        let pointers_per_block = self.superblock.pointers_per_block();
        let blocks_per_double_indirect = pointers_per_block * pointers_per_block;
        let double_indirect_index = block_index / blocks_per_double_indirect;

        self.resolve_indirect_ptr(triple_indirect_block, double_indirect_index)
            .and_then(|double_indirect_block_ptr| {
                self.resolve_double_indirect_ptr(
                    double_indirect_block_ptr,
                    block_index % blocks_per_double_indirect,
                )
            })
    }
}
