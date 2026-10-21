use kernel_device::block::{BlockDevice, MemoryBlockDevice};
use kernel_ext2::{Error, Ext2Fs, IndirectPointerLimits, MAX_NAME_LEN, RegularFile};

mod common;

generate_tests!(
    test_create_and_write_file:
    512 - test_create_and_write_file_standard,
    1 - test_create_and_write_file_tiny,
    32 - test_create_and_write_file_small,
    32768 - test_create_and_write_file_large,
    1048576 - test_create_and_write_file_huge,
);

fn test_create_and_write_file(sector_size: usize) {
    let mut fs = cow_fs!("kernel/ext2/tests/filesystems/empty.img", sector_size);

    let file_name = "my_file.txt";
    let mut root = fs.read_root_inode().unwrap();
    let mut file = fs.create_regular_file(&mut root, file_name).unwrap();
    assert!(
        fs.list_dir(&root)
            .unwrap()
            .iter()
            .find(|e| e.name() == Some(file_name))
            .is_some()
    );
    assert_eq!(file.len(), 0);

    let data = b"Hello, world!";
    let direct_capacity = 12 * fs.superblock().block_size() as usize;
    let repetitions = direct_capacity / data.len();
    for i in 0..repetitions {
        assert_eq!(
            fs.write_to_file(&mut file, i * data.len(), data).unwrap(),
            data.len()
        );
    }
    assert_eq!(file.len(), repetitions * data.len());

    let direct_blocks = file.inode().direct_ptrs().count() as u32;
    let expected_sectors = direct_blocks * fs.superblock().sectors_per_block();
    assert_eq!(file.inode().num_disk_sectors(), expected_sectors);
    let on_disk: RegularFile = fs
        .find_and_resolve_entry(&root, |e| e.name() == Some(file_name))
        .unwrap()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(on_disk.inode().num_disk_sectors(), expected_sectors);
    let mut read_back = vec![0_u8; on_disk.len()];
    assert_eq!(
        fs.read_from_file(&on_disk, 0, &mut read_back).unwrap(),
        read_back.len()
    );
    assert_eq!(data.repeat(repetitions), read_back);
}

generate_tests!(
    test_write_into_hole_counts_sectors:
    512 - test_write_into_hole_counts_sectors_standard,
    1024 - test_write_into_hole_counts_sectors_1024,
    4096 - test_write_into_hole_counts_sectors_4096,
);

fn test_write_into_hole_counts_sectors(sector_size: usize) {
    let mut fs = cow_fs!("kernel/ext2/tests/filesystems/large.img", sector_size);
    let root = fs.read_root_inode().unwrap();
    let mut file: RegularFile = fs
        .find_and_resolve_entry(&root, |e| e.name() == Some("sparse.bin"))
        .unwrap()
        .unwrap()
        .try_into()
        .unwrap();
    let before = file.inode().num_disk_sectors();
    let len_before = file.len();
    let block_size = fs.superblock().block_size() as usize;
    let free_before = fs.superblock().num_unallocated_blocks();

    let data = vec![0xCD_u8; block_size];
    assert_eq!(
        fs.write_to_file(&mut file, block_size, &data).unwrap(),
        block_size
    );
    assert_eq!(file.len(), len_before);
    let expected = before + fs.superblock().sectors_per_block();
    assert_eq!(file.inode().num_disk_sectors(), expected);
    let on_disk: RegularFile = fs
        .find_and_resolve_entry(&root, |e| e.name() == Some("sparse.bin"))
        .unwrap()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(on_disk.inode().num_disk_sectors(), expected);
    assert_eq!(fs.superblock().num_unallocated_blocks(), free_before - 1);
    assert_eq!(read_at(&fs, &on_disk, block_size, block_size), data);
}

generate_tests!(
    test_create_files:
    512 - test_create_files_standard,
    1 - test_create_files_tiny,
    32 - test_create_files_small,
    32768 - test_create_files_large,
    1048576 - test_create_files_huge,
);

fn test_create_files(sector_size: usize) {
    let mut fs = cow_fs!("kernel/ext2/tests/filesystems/empty.img", sector_size);

    let mut root = fs.read_root_inode().unwrap();
    for i in 0..25 {
        let file_name = format!("file_{}.txt", i);
        let file = fs.create_regular_file(&mut root, &file_name).unwrap();
        assert!(
            fs.list_dir(&root)
                .unwrap()
                .iter()
                .find(|e| e.name() == Some(&file_name))
                .is_some()
        );
        assert_eq!(file.len(), 0);
    }
}

generate_tests!(
    test_create_file_collision:
    512 - test_create_file_collision_standard,
    1 - test_create_file_collision_tiny,
    32 - test_create_file_collision_small,
    32768 - test_create_file_collision_large,
    1048576 - test_create_file_collision_huge,
);

fn test_create_file_collision(sector_size: usize) {
    let mut fs = cow_fs!("kernel/ext2/tests/filesystems/empty.img", sector_size);

    let mut root = fs.read_root_inode().unwrap();
    let file_name = "file.txt";
    let file = fs.create_regular_file(&mut root, file_name).unwrap();
    assert!(
        fs.list_dir(&root)
            .unwrap()
            .iter()
            .find(|e| e.name() == Some(file_name))
            .is_some()
    );
    assert_eq!(file.len(), 0);

    let mut root = fs.read_root_inode().unwrap();
    let free_inodes = fs.superblock().num_unallocated_inodes();
    let result = fs.create_regular_file(&mut root, file_name);
    assert_eq!(result.unwrap_err(), Error::EntryExists);
    assert_eq!(fs.superblock().num_unallocated_inodes(), free_inodes);
}

#[test]
fn test_create_file_name_length_limit() {
    let mut fs = cow_fs!("kernel/ext2/tests/filesystems/empty.img", 512);
    let mut root = fs.read_root_inode().unwrap();

    let max_name = "a".repeat(MAX_NAME_LEN);
    fs.create_regular_file(&mut root, &max_name).unwrap();
    let entries = fs.list_dir(&root).unwrap();
    assert!(entries.iter().any(|e| e.name() == Some(max_name.as_str())));

    let too_long = "b".repeat(MAX_NAME_LEN + 1);
    let result = fs.create_regular_file(&mut root, &too_long);
    assert_eq!(result.unwrap_err(), Error::NameTooLong);
    let entries = fs.list_dir(&root).unwrap();
    assert!(entries.iter().all(|e| e.name() != Some("")));
}

generate_tests!(
    test_allocate_skips_used_slots:
    512 - test_allocate_skips_used_slots_standard,
    1 - test_allocate_skips_used_slots_tiny,
    32 - test_allocate_skips_used_slots_small,
    32768 - test_allocate_skips_used_slots_large,
    1048576 - test_allocate_skips_used_slots_huge,
);

fn test_allocate_skips_used_slots(sector_size: usize) {
    let mut fs = cow_fs!("kernel/ext2/tests/filesystems/empty.img", sector_size);
    // empty.img (dumpe2fs): inodes 1-11 used, 11 is lost+found; blocks 1-53 used
    let free_inodes = fs.superblock().num_unallocated_inodes();
    let free_blocks = fs.superblock().num_unallocated_blocks();

    let first = fs.allocate_inode().unwrap().unwrap();
    let second = fs.allocate_inode().unwrap().unwrap();
    assert_eq!(first.get(), 12);
    assert_eq!(second.get(), 13);
    assert_eq!(fs.superblock().num_unallocated_inodes(), free_inodes - 2);
    assert_eq!(fs.superblock().num_unallocated_blocks(), free_blocks);

    let first = fs.allocate_block().unwrap().unwrap();
    let second = fs.allocate_block().unwrap().unwrap();
    assert_eq!(first.get(), 54);
    assert_eq!(second.get(), 55);
    assert_eq!(fs.superblock().num_unallocated_blocks(), free_blocks - 2);
    assert_eq!(fs.superblock().num_unallocated_inodes(), free_inodes - 2);
}

// The raw descriptor bytes are the oracle because a mount that reads the table
// at a different offset than allocate_block writes it would never see the drop.
#[test]
fn test_allocate_block_updates_group_descriptor_1k_blocks() {
    allocate_block_updates_group_descriptor("kernel/ext2/tests/filesystems/empty.img", 2048);
}

#[test]
fn test_allocate_block_updates_group_descriptor_4k_blocks() {
    allocate_block_updates_group_descriptor("kernel/ext2/tests/filesystems/empty4k.img", 4096);
}

fn allocate_block_updates_group_descriptor(image: &str, bgdt_offset: usize) {
    const BG_FREE_BLOCKS_COUNT: usize = 12;
    let raw_free_blocks = |image: &[u8]| {
        let at = bgdt_offset + BG_FREE_BLOCKS_COUNT;
        u16::from_le_bytes([image[at], image[at + 1]])
    };

    let mut image_data = common::load_copy_of_image(image);
    let free_before = raw_free_blocks(&image_data);

    let device = MemoryBlockDevice::try_new(512, &mut image_data[..]).unwrap();
    let mut fs = Ext2Fs::try_new(device).unwrap();
    let first = fs.allocate_block().unwrap().unwrap();
    drop(fs);
    assert_eq!(raw_free_blocks(&image_data), free_before - 1);

    let device = MemoryBlockDevice::try_new(512, &mut image_data[..]).unwrap();
    let mut fs = Ext2Fs::try_new(device).unwrap();
    assert_eq!(
        fs.superblock().num_unallocated_blocks(),
        u32::from(free_before) - 1
    );
    let second = fs.allocate_block().unwrap().unwrap();
    assert_eq!(second.get(), first.get() + 1);
    drop(fs);
    assert_eq!(raw_free_blocks(&image_data), free_before - 2);
}

fn create_file<T: BlockDevice>(fs: &mut Ext2Fs<T>, name: &str) -> RegularFile {
    let mut root = fs.read_root_inode().unwrap();
    fs.create_regular_file(&mut root, name).unwrap()
}

fn on_disk<T: BlockDevice>(fs: &Ext2Fs<T>, file: &RegularFile) -> RegularFile {
    fs.read_inode(file.inode_address())
        .unwrap()
        .try_into()
        .unwrap()
}

fn read_all<T: BlockDevice>(fs: &Ext2Fs<T>, file: &RegularFile) -> Vec<u8> {
    let mut buf = vec![0_u8; file.len()];
    assert_eq!(fs.read_from_file(file, 0, &mut buf).unwrap(), file.len());
    buf
}

generate_tests!(
    empty_write_returns_zero_and_changes_nothing:
    1024 - empty_write_returns_zero_and_changes_nothing_1k,
    4096 - empty_write_returns_zero_and_changes_nothing_4k,
);

fn empty_write_returns_zero_and_changes_nothing(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    fs.write_to_file(&mut file, 0, b"abc").unwrap();
    let free_blocks = fs.superblock().num_unallocated_blocks();
    let sectors = file.num_disk_sectors();

    for offset in [0, block_size, 3 * block_size + 5] {
        assert_eq!(fs.write_to_file(&mut file, offset, &[]).unwrap(), 0);
    }

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_blocks);
    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), 3);
    assert_eq!(stored.num_disk_sectors(), sectors);
    assert_eq!(read_all(&fs, &stored), b"abc");
}

generate_tests!(
    write_past_max_file_size_fails_without_allocating:
    1024 - write_past_max_file_size_fails_without_allocating_1k,
    4096 - write_past_max_file_size_fails_without_allocating_4k,
);

fn write_past_max_file_size_fails_without_allocating(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_blocks = fs.superblock().num_unallocated_blocks();
    let IndirectPointerLimits {
        double_indirect_end,
        ..
    } = fs.indirect_pointer_limits();
    let pointers_per_block = (block_size / 4) as u64;
    let triple_indirect_end = u64::from(double_indirect_end) + pointers_per_block.pow(3);
    let max_bytes = usize::try_from(triple_indirect_end).unwrap() * block_size;

    for (offset, data) in [
        (max_bytes, &b"x"[..]),
        (max_bytes - 1, &b"xy"[..]),
        (usize::MAX - 1, &b"xyz"[..]),
    ] {
        assert_eq!(
            fs.write_to_file(&mut file, offset, data),
            Err(Error::FileTooLarge),
            "offset {offset}"
        );
    }

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_blocks);
    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), 0);
    assert_eq!(stored.num_disk_sectors(), 0);
}

generate_tests!(
    write_at_block_boundary_starts_next_block:
    1024 - write_at_block_boundary_starts_next_block_1k,
    4096 - write_at_block_boundary_starts_next_block_4k,
);

fn write_at_block_boundary_starts_next_block(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let sectors_per_block = fs.superblock().sectors_per_block();
    let first = vec![0xAA_u8; block_size];
    fs.write_to_file(&mut file, 0, &first).unwrap();
    assert_eq!(file.num_disk_sectors(), sectors_per_block);

    assert_eq!(
        fs.write_to_file(&mut file, block_size, b"0123456789")
            .unwrap(),
        10
    );

    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), block_size + 10);
    assert_eq!(stored.num_disk_sectors(), 2 * sectors_per_block);
    assert_eq!(read_all(&fs, &stored), [&first[..], b"0123456789"].concat());
}

generate_tests!(
    write_straddling_two_blocks_zero_fills_the_rest:
    1024 - write_straddling_two_blocks_zero_fills_the_rest_1k,
    4096 - write_straddling_two_blocks_zero_fills_the_rest_4k,
);

fn write_straddling_two_blocks_zero_fills_the_rest(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_blocks = fs.superblock().num_unallocated_blocks();

    let data = [0x5A_u8; 20];
    assert_eq!(
        fs.write_to_file(&mut file, block_size - 10, &data).unwrap(),
        20
    );

    assert_eq!(fs.superblock().num_unallocated_blocks(), free_blocks - 2);
    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), block_size + 10);
    let mut expected = vec![0_u8; block_size - 10];
    expected.extend_from_slice(&data);
    assert_eq!(read_all(&fs, &stored), expected);
}

generate_tests!(
    overwrite_inside_block_preserves_other_bytes:
    1024 - overwrite_inside_block_preserves_other_bytes_1k,
    4096 - overwrite_inside_block_preserves_other_bytes_4k,
);

fn overwrite_inside_block_preserves_other_bytes(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let mut expected = vec![0xAA_u8; 2 * block_size];
    fs.write_to_file(&mut file, 0, &expected).unwrap();
    let free_blocks = fs.superblock().num_unallocated_blocks();
    let sectors = file.num_disk_sectors();

    assert_eq!(
        fs.write_to_file(&mut file, 100, b"\x11\x22\x33\x44")
            .unwrap(),
        4
    );

    expected[100..104].copy_from_slice(b"\x11\x22\x33\x44");
    assert_eq!(fs.superblock().num_unallocated_blocks(), free_blocks);
    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), 2 * block_size);
    assert_eq!(stored.num_disk_sectors(), sectors);
    assert_eq!(read_all(&fs, &stored), expected);
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn read_at<T: BlockDevice>(
    fs: &Ext2Fs<T>,
    file: &RegularFile,
    offset: usize,
    len: usize,
) -> Vec<u8> {
    let mut buf = vec![0xEE_u8; len];
    assert_eq!(fs.read_from_file(file, offset, &mut buf).unwrap(), len);
    buf
}

generate_tests!(
    write_past_direct_blocks_allocates_single_indirect:
    1024 - write_past_direct_blocks_allocates_single_indirect_1k,
    4096 - write_past_direct_blocks_allocates_single_indirect_4k,
);

fn write_past_direct_blocks_allocates_single_indirect(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_blocks = fs.superblock().num_unallocated_blocks();
    let sectors_per_block = fs.superblock().sectors_per_block();
    let data = pattern(13 * block_size + 7);

    assert_eq!(fs.write_to_file(&mut file, 0, &data).unwrap(), data.len());

    let data_blocks = 14;
    let tables = 1;
    assert_eq!(
        fs.superblock().num_unallocated_blocks(),
        free_blocks - data_blocks - tables
    );
    let stored = on_disk(&fs, &file);
    assert!(stored.single_indirect_ptr().is_some());
    assert_eq!(stored.double_indirect_ptr(), None);
    assert_eq!(
        stored.num_disk_sectors(),
        (data_blocks + tables) * sectors_per_block
    );
    assert_eq!(read_all(&fs, &stored), data);
}

generate_tests!(
    sparse_write_in_double_indirect_region_allocates_each_table:
    1024 - sparse_write_in_double_indirect_region_allocates_each_table_1k,
    4096 - sparse_write_in_double_indirect_region_allocates_each_table_4k,
);

fn sparse_write_in_double_indirect_region_allocates_each_table(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_blocks = fs.superblock().num_unallocated_blocks();
    let sectors_per_block = fs.superblock().sectors_per_block();
    let IndirectPointerLimits { indirect_end, .. } = fs.indirect_pointer_limits();
    let pointers_per_block = (block_size / 4) as u32;
    // second table under the double indirect block, so the slot math is exercised
    let block_index = indirect_end + pointers_per_block + 3;
    let offset = block_index as usize * block_size + 5;
    let data = pattern(block_size);

    assert_eq!(
        fs.write_to_file(&mut file, offset, &data).unwrap(),
        data.len()
    );

    let allocated = 2 + 2; // two data blocks, the double table and one single table
    assert_eq!(
        fs.superblock().num_unallocated_blocks(),
        free_blocks - allocated
    );
    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), offset + data.len());
    assert_eq!(stored.num_disk_sectors(), allocated * sectors_per_block);
    assert!(stored.direct_ptrs().all(|ptr| ptr.is_none()));
    assert_eq!(stored.single_indirect_ptr(), None);
    assert!(stored.double_indirect_ptr().is_some());
    assert_eq!(stored.triple_indirect_ptr(), None);
    assert!(!fs.is_block_allocated(&stored, block_index - 1).unwrap());
    assert!(fs.is_block_allocated(&stored, block_index).unwrap());
    assert!(fs.is_block_allocated(&stored, block_index + 1).unwrap());

    let mut expected = vec![0_u8; offset];
    expected.extend_from_slice(&data);
    assert_eq!(read_all(&fs, &stored), expected);
}

generate_tests!(
    sparse_write_in_triple_indirect_region_allocates_each_table:
    1024 - sparse_write_in_triple_indirect_region_allocates_each_table_1k,
    4096 - sparse_write_in_triple_indirect_region_allocates_each_table_4k,
);

fn sparse_write_in_triple_indirect_region_allocates_each_table(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_blocks = fs.superblock().num_unallocated_blocks();
    let sectors_per_block = fs.superblock().sectors_per_block();
    let IndirectPointerLimits {
        double_indirect_end,
        ..
    } = fs.indirect_pointer_limits();
    let pointers_per_block = (block_size / 4) as u32;
    // a non-first slot at every level of the triple indirect tree
    let block_index =
        double_indirect_end + pointers_per_block * pointers_per_block + pointers_per_block + 1;
    let offset = block_index as usize * block_size;
    let data = pattern(100);

    assert_eq!(
        fs.write_to_file(&mut file, offset, &data).unwrap(),
        data.len()
    );

    let allocated = 1 + 3;
    assert_eq!(
        fs.superblock().num_unallocated_blocks(),
        free_blocks - allocated
    );
    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), offset + data.len());
    assert_eq!(stored.num_disk_sectors(), allocated * sectors_per_block);
    assert_eq!(stored.single_indirect_ptr(), None);
    assert_eq!(stored.double_indirect_ptr(), None);
    assert!(stored.triple_indirect_ptr().is_some());
    assert!(!fs.is_block_allocated(&stored, double_indirect_end).unwrap());
    assert!(!fs.is_block_allocated(&stored, block_index - 1).unwrap());
    assert!(fs.is_block_allocated(&stored, block_index).unwrap());
    assert_eq!(read_at(&fs, &stored, offset, data.len()), data);
    assert_eq!(
        read_at(&fs, &stored, offset - block_size, block_size),
        vec![0; block_size]
    );
    assert_eq!(read_at(&fs, &stored, 0, block_size), vec![0; block_size]);
}

generate_tests!(
    write_until_full_persists_every_allocated_block:
    1024 - write_until_full_persists_every_allocated_block_1k,
    4096 - write_until_full_persists_every_allocated_block_4k,
);

fn write_until_full_persists_every_allocated_block(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "f");
    let free_blocks = fs.superblock().num_unallocated_blocks();
    let sectors_per_block = fs.superblock().sectors_per_block();
    let data = pattern((free_blocks as usize + 8) * block_size);

    let written = fs.write_to_file(&mut file, 0, &data).unwrap();

    assert!(
        written > 0 && written < data.len(),
        "short write, got {written}"
    );
    assert_eq!(written % block_size, 0);
    assert_eq!(fs.superblock().num_unallocated_blocks(), 0);
    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), written);
    assert_eq!(stored.num_disk_sectors(), free_blocks * sectors_per_block);
    assert_eq!(read_all(&fs, &stored), data[..written]);

    assert_eq!(
        fs.write_to_file(&mut file, written, b"more"),
        Err(Error::NoSpace)
    );
    let stored = on_disk(&fs, &file);
    assert_eq!(stored.len(), written);
    assert_eq!(stored.num_disk_sectors(), free_blocks * sectors_per_block);
}

generate_tests!(
    list_dir_skips_deleted_entries:
    1024 - list_dir_skips_deleted_entries_1k,
    4096 - list_dir_skips_deleted_entries_4k,
);

fn list_dir_skips_deleted_entries(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    create_file(&mut fs, "a");
    create_file(&mut fs, "b");
    let mut root = fs.read_root_inode().unwrap();
    let block = root.direct_ptrs().next().flatten().unwrap();
    let mut data = vec![0_u8; block_size];
    fs.read_block(block, &mut data).unwrap();
    let mut offset = 0;
    while &data[offset + 8..offset + 8 + usize::from(data[offset + 6])] != b"a" {
        offset += usize::from(u16::from_le_bytes([data[offset + 4], data[offset + 5]]));
    }
    data[offset..offset + 4].fill(0);
    fs.write_block(block, &data).unwrap();

    assert_eq!(entry_names(&fs), [".", "..", "lost+found", "b"]);

    fs.create_regular_file(&mut root, "c").unwrap();
    assert_eq!(entry_names(&fs), [".", "..", "lost+found", "c", "b"]);
}

fn entry_names<T: BlockDevice>(fs: &Ext2Fs<T>) -> Vec<String> {
    let root = fs.read_root_inode().unwrap();
    fs.list_dir(&root)
        .unwrap()
        .iter()
        .map(|e| e.name().unwrap().to_owned())
        .collect()
}

generate_tests!(
    created_file_reads_back_through_a_fresh_lookup:
    1024 - created_file_reads_back_through_a_fresh_lookup_1k,
    4096 - created_file_reads_back_through_a_fresh_lookup_4k,
);

fn created_file_reads_back_through_a_fresh_lookup(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let mut file = create_file(&mut fs, "data.bin");
    let data = pattern(13 * block_size + 7);
    fs.write_to_file(&mut file, 0, &data).unwrap();

    let root = fs.read_root_inode().unwrap();
    let found: RegularFile = fs
        .find_and_resolve_entry(&root, |e| e.name() == Some("data.bin"))
        .unwrap()
        .unwrap()
        .try_into()
        .unwrap();

    assert_eq!(found.num_hard_links(), 1);
    assert_eq!(found.len(), data.len());
    assert!(found.single_indirect_ptr().is_some());
    assert_eq!(
        found.num_disk_sectors(),
        15 * fs.superblock().sectors_per_block()
    );
    assert_eq!(read_all(&fs, &found), data);
}

generate_tests!(
    creating_past_the_first_directory_block_grows_the_directory:
    1024 - creating_past_the_first_directory_block_grows_the_directory_1k,
    4096 - creating_past_the_first_directory_block_grows_the_directory_4k,
);

fn creating_past_the_first_directory_block_grows_the_directory(block_size: usize) {
    let mut fs = empty_fs!(block_size);
    let sectors_per_block = fs.superblock().sectors_per_block();
    let mut root = fs.read_root_inode().unwrap();
    assert_eq!(root.len(), block_size);
    let mut names = vec![];
    while root.len() == block_size {
        let name = format!("{}{}", names.len(), "n".repeat(MAX_NAME_LEN - 3));
        fs.create_regular_file(&mut root, &name).unwrap();
        names.push(name);
        root = fs.read_root_inode().unwrap();
    }

    assert_eq!(root.len(), 2 * block_size);
    assert_eq!(root.num_disk_sectors(), 2 * sectors_per_block);
    let listed = entry_names(&fs);
    assert!(names.iter().all(|name| listed.contains(name)));
}

#[test]
fn directory_past_direct_blocks_is_not_supported_and_frees_the_inode() {
    let mut fs = empty_fs!(1024);
    let mut root = fs.read_root_inode().unwrap();
    let result = loop {
        let name = format!(
            "{:03}{}",
            entry_names(&fs).len(),
            "n".repeat(MAX_NAME_LEN - 3)
        );
        let free_inodes = fs.superblock().num_unallocated_inodes();
        let free_blocks = fs.superblock().num_unallocated_blocks();
        match fs.create_regular_file(&mut root, &name) {
            Ok(_) => {}
            Err(e) => {
                assert_eq!(fs.superblock().num_unallocated_inodes(), free_inodes);
                assert_eq!(fs.superblock().num_unallocated_blocks(), free_blocks);
                break e;
            }
        }
    };

    assert_eq!(result, Error::NotSupported);
    assert_eq!(fs.read_root_inode().unwrap().len(), 12 * 1024);
}
