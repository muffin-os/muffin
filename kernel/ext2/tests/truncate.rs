use kernel_device::block::{BlockDevice, MemoryBlockDevice};
use kernel_ext2::{BlockAddress, Error, Ext2Fs, IndirectPointerLimits, InodeAddress, RegularFile};

mod common;

fn create_file<T: BlockDevice>(fs: &mut Ext2Fs<T>, name: &str) -> RegularFile {
    let mut root = fs.read_root_inode().unwrap();
    fs.create_regular_file(&mut root, name).unwrap()
}

fn open_inode<T: BlockDevice>(fs: &Ext2Fs<T>, address: InodeAddress) -> RegularFile {
    fs.read_inode(address).unwrap().try_into().unwrap()
}

fn read_all<T: BlockDevice>(fs: &Ext2Fs<T>, file: &RegularFile) -> Vec<u8> {
    let mut buf = vec![0xEE_u8; file.len()];
    assert_eq!(fs.read_from_file(file, 0, &mut buf).unwrap(), file.len());
    buf
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn empty_image(block_size: usize) -> Vec<u8> {
    common::load_copy_of_image(match block_size {
        1024 => "kernel/ext2/tests/filesystems/empty.img",
        4096 => "kernel/ext2/tests/filesystems/empty4k.img",
        other => panic!("no empty fixture with {other} byte blocks"),
    })
}

fn assert_no_blocks<T: BlockDevice>(fs: &Ext2Fs<T>, file: &RegularFile) {
    let stored = open_inode(fs, file.inode_address());
    assert_eq!(stored.len(), 0);
    assert_eq!(stored.num_disk_sectors(), 0);
    assert!(stored.direct_ptrs().all(|ptr| ptr.is_none()));
    assert_eq!(stored.single_indirect_ptr(), None);
    assert_eq!(stored.double_indirect_ptr(), None);
    assert_eq!(stored.triple_indirect_ptr(), None);
}

generate_tests!(
    deallocate_block_frees_bitmap_bit_and_counters:
    1024 - deallocate_block_frees_bitmap_bit_and_counters_1k,
    4096 - deallocate_block_frees_bitmap_bit_and_counters_4k,
);

// The raw descriptor bytes are the oracle, as in the allocate_block test.
fn deallocate_block_frees_bitmap_bit_and_counters(block_size: usize) {
    const BG_FREE_BLOCKS_COUNT: usize = 12;
    let at = block_size.max(2048) + BG_FREE_BLOCKS_COUNT;
    let raw_free_blocks = |image: &[u8]| u16::from_le_bytes([image[at], image[at + 1]]);
    let mut image = empty_image(block_size);
    let group_free_before = raw_free_blocks(&image);

    let device = MemoryBlockDevice::try_new(512, &mut image[..]).unwrap();
    let mut fs = Ext2Fs::try_new(device).unwrap();
    let free_before = fs.superblock().num_unallocated_blocks();
    let block = fs.allocate_block().unwrap().unwrap();

    fs.deallocate_block(block).unwrap();

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before);
    assert_eq!(
        fs.deallocate_block(block),
        Err(Error::InvalidBlockAddress(block.get()))
    );
    let past_end = BlockAddress::new(fs.superblock().num_blocks()).unwrap();
    assert_eq!(
        fs.deallocate_block(past_end),
        Err(Error::InvalidBlockAddress(past_end.get()))
    );
    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before);
    drop(fs);
    assert_eq!(raw_free_blocks(&image), group_free_before);

    let device = MemoryBlockDevice::try_new(512, &mut image[..]).unwrap();
    let mut fs = Ext2Fs::try_new(device).unwrap();
    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before);
    assert_eq!(fs.allocate_block().unwrap(), Some(block));
}

generate_tests!(
    truncate_to_zero_frees_every_block_and_table:
    1024 - truncate_to_zero_frees_every_block_and_table_1k,
    4096 - truncate_to_zero_frees_every_block_and_table_4k,
);

fn truncate_to_zero_frees_every_block_and_table(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_before = fs.superblock().num_unallocated_blocks();
    fs.write_to_file(&mut file, 0, &pattern(20 * block_size))
        .unwrap();
    assert!(file.single_indirect_ptr().is_some());

    fs.truncate_file(&mut file, 0).unwrap();

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before);
    assert_eq!(file.len(), 0);
    assert_no_blocks(&fs, &file);
}

generate_tests!(
    truncate_file_with_every_tier_frees_every_table:
    1024 - truncate_file_with_every_tier_frees_every_table_1k,
    4096 - truncate_file_with_every_tier_frees_every_table_4k,
);

fn truncate_file_with_every_tier_frees_every_table(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_before = fs.superblock().num_unallocated_blocks();
    let IndirectPointerLimits {
        indirect_end,
        double_indirect_end,
        ..
    } = fs.indirect_pointer_limits();
    let pointers_per_block = (block_size / 4) as u32;
    for block_index in [
        0,
        indirect_end - 1,
        indirect_end + pointers_per_block + 1,
        double_indirect_end + pointers_per_block * pointers_per_block + 1,
    ] {
        fs.write_to_file(&mut file, block_index as usize * block_size, b"x")
            .unwrap();
    }
    let stored = open_inode(&fs, file.inode_address());
    assert!(stored.single_indirect_ptr().is_some());
    assert!(stored.double_indirect_ptr().is_some());
    assert!(stored.triple_indirect_ptr().is_some());
    // 4 data blocks, 1 single, 1 double + 1 single, 1 triple + 1 double + 1 single
    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before - 10);

    fs.truncate_file(&mut file, 0).unwrap();

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before);
    assert_no_blocks(&fs, &file);
}

generate_tests!(
    truncate_inside_double_tier_frees_tables_left_empty:
    1024 - truncate_inside_double_tier_frees_tables_left_empty_1k,
    4096 - truncate_inside_double_tier_frees_tables_left_empty_4k,
);

fn truncate_inside_double_tier_frees_tables_left_empty(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_before = fs.superblock().num_unallocated_blocks();
    let sectors_per_block = fs.superblock().sectors_per_block();
    let IndirectPointerLimits { indirect_end, .. } = fs.indirect_pointer_limits();
    let pointers_per_block = (block_size / 4) as u32;
    let first = indirect_end + 1;
    let second = indirect_end + pointers_per_block + 1;
    for block_index in [first, second] {
        fs.write_to_file(&mut file, block_index as usize * block_size, b"x")
            .unwrap();
    }
    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before - 5);

    let second_table_begin = (indirect_end + pointers_per_block) as usize * block_size;
    fs.truncate_file(&mut file, second_table_begin).unwrap();

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before - 3);
    let stored = open_inode(&fs, file.inode_address());
    assert_eq!(stored.len(), second_table_begin);
    assert_eq!(stored.num_disk_sectors(), 3 * sectors_per_block);
    assert!(stored.double_indirect_ptr().is_some());
    assert!(fs.is_block_allocated(&stored, first).unwrap());
    assert!(!fs.is_block_allocated(&stored, second).unwrap());

    // the only data left sits past the new end, so the whole double tree empties
    fs.truncate_file(&mut file, first as usize * block_size)
        .unwrap();

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before);
    let stored = open_inode(&fs, file.inode_address());
    assert_eq!(stored.num_disk_sectors(), 0);
    assert_eq!(stored.double_indirect_ptr(), None);
}

generate_tests!(
    truncate_to_mid_block_zeroes_the_tail:
    1024 - truncate_to_mid_block_zeroes_the_tail_1k,
    4096 - truncate_to_mid_block_zeroes_the_tail_4k,
);

fn truncate_to_mid_block_zeroes_the_tail(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_before = fs.superblock().num_unallocated_blocks();
    let sectors_per_block = fs.superblock().sectors_per_block();
    let data = pattern(3 * block_size);
    fs.write_to_file(&mut file, 0, &data).unwrap();

    fs.truncate_file(&mut file, block_size + 10).unwrap();

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before - 2);
    let stored = open_inode(&fs, file.inode_address());
    assert_eq!(stored.len(), block_size + 10);
    assert_eq!(stored.num_disk_sectors(), 2 * sectors_per_block);
    assert_eq!(read_all(&fs, &stored), data[..block_size + 10]);

    fs.truncate_file(&mut file, 3 * block_size).unwrap();

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before - 2);
    let mut expected = data[..block_size + 10].to_vec();
    expected.resize(3 * block_size, 0);
    assert_eq!(
        read_all(&fs, &open_inode(&fs, file.inode_address())),
        expected
    );
}

generate_tests!(
    truncate_to_larger_size_is_sparse:
    1024 - truncate_to_larger_size_is_sparse_1k,
    4096 - truncate_to_larger_size_is_sparse_4k,
);

fn truncate_to_larger_size_is_sparse(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    fs.write_to_file(&mut file, 0, b"0123456789").unwrap();
    let free_before = fs.superblock().num_unallocated_blocks();
    let sectors = file.num_disk_sectors();
    let len = 100 * block_size + 3;

    fs.truncate_file(&mut file, len).unwrap();

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before);
    let stored = open_inode(&fs, file.inode_address());
    assert_eq!(stored.len(), len);
    assert_eq!(stored.num_disk_sectors(), sectors);
    let mut expected = b"0123456789".to_vec();
    expected.resize(len, 0);
    assert_eq!(read_all(&fs, &stored), expected);
}

generate_tests!(
    truncate_past_max_file_size_fails:
    1024 - truncate_past_max_file_size_fails_1k,
    4096 - truncate_past_max_file_size_fails_4k,
);

fn truncate_past_max_file_size_fails(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");

    assert_eq!(
        fs.truncate_file(&mut file, usize::MAX),
        Err(Error::FileTooLarge)
    );

    assert_eq!(open_inode(&fs, file.inode_address()).len(), 0);
}

generate_tests!(
    truncate_to_same_size_leaves_the_disk_untouched:
    1024 - truncate_to_same_size_leaves_the_disk_untouched_1k,
    4096 - truncate_to_same_size_leaves_the_disk_untouched_4k,
);

fn truncate_to_same_size_leaves_the_disk_untouched(block_size: usize) {
    let mut image = empty_image(block_size);
    let device = MemoryBlockDevice::try_new(512, &mut image[..]).unwrap();
    let mut fs = Ext2Fs::try_new(device).unwrap();
    let mut file = create_file(&mut fs, "f");
    let len = 2 * block_size + 5;
    fs.write_to_file(&mut file, 0, &pattern(len)).unwrap();
    let address = file.inode_address();
    drop(fs);
    let before = image.clone();

    let device = MemoryBlockDevice::try_new(512, &mut image[..]).unwrap();
    let mut fs = Ext2Fs::try_new(device).unwrap();
    let mut file = open_inode(&fs, address);
    fs.truncate_file(&mut file, len).unwrap();
    drop(fs);

    assert!(
        image == before,
        "truncate to the same size wrote to the disk"
    );
}
