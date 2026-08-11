use alloc::vec;
use alloc::vec::Vec;

use kernel_device::block::BlockDevice;

use crate::{BlockAddress, Error, Ext2Fs, Inode, RegularFile};

const SZ: usize = size_of::<BlockAddress>();
const INDIRECT_CACHE_CAPACITY: usize = 4;

/// Four entries cover a triple indirect resolution, which touches three
/// distinct blocks, so a sequential scan never evicts a block it is about
/// to need again.
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
        let table = &self.entries[0].1;
        debug_assert!(
            index < table.len(),
            "indirect pointer index must be within the pointer table"
        );
        Some(table[index])
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
        if offset >= file_size {
            return Ok(0);
        }

        let block_size = self.superblock.block_size();
        let offset = offset as u32;

        let start_block = offset / block_size;
        let end_block = (offset + buf.len() as u32 - 1) / block_size;
        let relative_offset = (offset % block_size) as usize;
        let block_count = (end_block - start_block + 1) as usize;

        // read blocks
        let mut data: Vec<u8> = vec![0_u8; block_count * block_size as usize]; // TODO: avoid allocation - maybe try to only allocate the first and last block if the read is not aligned, but read the rest directly into the buffer
        let res =
            self.read_blocks_from_inode(file, start_block as usize, end_block as usize, &mut data)?;
        // copy the data into buf, but only the requested part and only up to the file size
        let total_read = res.min(file_size - offset as usize).min(buf.len());
        buf[..total_read].copy_from_slice(&data[relative_offset..relative_offset + total_read]);

        Ok(total_read)
    }

    pub(crate) fn read_blocks_from_inode(
        &self,
        inode: &Inode,
        start_block: usize,
        end_block: usize,
        buf: &mut [u8],
    ) -> Result<usize, Error> {
        let block_size = self.superblock.block_size() as usize;
        assert_eq!(
            buf.len(),
            (end_block - start_block + 1) * block_size,
            "buf.len() must be equal to the number of blocks you want to read"
        );

        let mut pointers = Vec::with_capacity(end_block - start_block + 1);
        for block in start_block..=end_block {
            pointers.push(self.resolve_block_index(inode, block as u32)?);
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

    pub fn indirect_pointer_limits(&self) -> (u32, u32, u32) {
        let pointers_per_block = self.superblock.block_size() / 4;
        let direct_limit = 12;
        let indirect_limit = direct_limit + pointers_per_block;
        let double_indirect_limit = indirect_limit + pointers_per_block * pointers_per_block;
        (direct_limit, indirect_limit, double_indirect_limit)
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
        let (direct_limit, indirect_limit, double_indirect_limit) = self.indirect_pointer_limits();

        Ok(if block_index < direct_limit {
            inode.direct_ptrs().nth(block_index as usize).flatten()
        } else if block_index < indirect_limit {
            self.resolve_indirect_ptr(inode.single_indirect_ptr(), block_index - direct_limit)?
        } else if block_index < double_indirect_limit {
            self.resolve_double_indirect_ptr(
                inode.double_indirect_ptr(),
                block_index - indirect_limit,
            )?
        } else {
            self.resolve_triple_indirect_ptr(
                inode.triple_indirect_ptr(),
                block_index - double_indirect_limit,
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

        debug_assert!(
            index < table.len(),
            "indirect pointer index must be within the pointer table"
        );
        let resolved = table[index];
        self.indirect_cache.lock().insert(indirect_ptr, table);
        Ok(resolved)
    }

    pub fn resolve_double_indirect_ptr(
        &self,
        double_indirect_block: Option<BlockAddress>,
        block_index: u32,
    ) -> Result<Option<BlockAddress>, Error> {
        let block_size = self.superblock.block_size();

        let single_indirect_block_size = block_size / 4;
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
        let pointers_per_block = self.superblock.block_size() / 4;
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
