use kernel_device::block::MemoryBlockDevice;
use kernel_ext2::{Error, Ext2Fs, RegularFile};

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
    // write `data` until all direct pointers are used
    for i in 0..((1024 * 12) / data.len()) {
        assert_eq!(
            fs.write_to_file(&mut file, i * data.len(), data).unwrap(),
            data.len()
        );
    }

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
    let result = fs.create_regular_file(&mut root, file_name);
    assert_eq!(result.unwrap_err(), Error::EntryExists);
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
