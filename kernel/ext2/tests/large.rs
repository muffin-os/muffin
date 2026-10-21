use std::cell::Cell;
use std::rc::Rc;

use kernel_device::block::{BlockDevice, MemoryBlockDevice};
use kernel_ext2::{Ext2Fs, IndirectPointerLimits, RegularFile};

mod common;

const IMAGE: &str = "kernel/ext2/tests/filesystems/large.img";
const PATTERN_LEN: usize = 600_000;
const HUGE_LEN: usize = 160 * 1024 * 1024;
const TRIPLE_BOUNDARY_WINDOW: (usize, usize) = (65804 * 1024 - 2048, 6144);
const INNER_DOUBLE_WINDOW: (usize, usize) = ((65804 + 65536 - 1) * 1024, 3072);

fn pattern_byte(offset: usize) -> u8 {
    (offset % 251) as u8
}

fn first_mismatch(data: &[u8], file_offset: usize) -> Option<usize> {
    data.iter()
        .enumerate()
        .find(|(i, b)| **b != pattern_byte(file_offset + i))
        .map(|(i, _)| file_offset + i)
}

fn open_pattern<T>(fs: &Ext2Fs<T>) -> RegularFile
where
    T: BlockDevice,
{
    let root = fs.read_root_inode().expect("root inode must be readable");
    fs.find_and_resolve_entry(&root, |e| e.name().is_some_and(|n| n == "pattern.bin"))
        .expect("directory lookup must succeed")
        .expect("pattern.bin must exist in the image")
        .try_into()
        .expect("pattern.bin must be a regular file")
}

fn open_huge<T>(fs: &Ext2Fs<T>) -> RegularFile
where
    T: BlockDevice,
{
    let root = fs.read_root_inode().expect("root inode must be readable");
    fs.find_and_resolve_entry(&root, |e| e.name().is_some_and(|n| n == "huge.bin"))
        .expect("directory lookup must succeed")
        .expect("huge.bin must exist in the image")
        .try_into()
        .expect("huge.bin must be a regular file")
}

fn read_exact_at<T>(fs: &Ext2Fs<T>, file: &RegularFile, offset: usize, len: usize) -> Vec<u8>
where
    T: BlockDevice,
{
    let mut buf = vec![0_u8; len];
    let read = fs
        .read_from_file(file, offset, &mut buf)
        .expect("read_from_file must succeed");
    assert_eq!(len, read, "short read at offset {offset} of {len} bytes");
    buf
}

generate_tests!(
    read_whole_pattern_file:
    512 - read_whole_pattern_file_512,
    1024 - read_whole_pattern_file_1024,
    4096 - read_whole_pattern_file_4096,
);

fn read_whole_pattern_file(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/large.img", sector_size);
    let file = open_pattern(&fs);
    assert_eq!(PATTERN_LEN, file.len(), "unexpected pattern.bin size");

    let data = read_exact_at(&fs, &file, 0, PATTERN_LEN);
    assert_eq!(
        None,
        first_mismatch(&data, 0),
        "pattern mismatch in full file read at sector size {sector_size}"
    );
}

generate_tests!(
    read_across_direct_boundary:
    512 - read_across_direct_boundary_512,
    1024 - read_across_direct_boundary_1024,
    4096 - read_across_direct_boundary_4096,
);

fn read_across_direct_boundary(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/large.img", sector_size);
    let file = open_pattern(&fs);

    let offset = 11 * 1024 + 100;
    let data = read_exact_at(&fs, &file, offset, 3000);
    assert_eq!(
        None,
        first_mismatch(&data, offset),
        "pattern mismatch across the direct to indirect boundary at sector size {sector_size}"
    );
}

generate_tests!(
    read_across_indirect_boundary:
    512 - read_across_indirect_boundary_512,
    1024 - read_across_indirect_boundary_1024,
    4096 - read_across_indirect_boundary_4096,
);

fn read_across_indirect_boundary(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/large.img", sector_size);
    let file = open_pattern(&fs);

    let offset = 267 * 1024 + 100;
    let data = read_exact_at(&fs, &file, offset, 3000);
    assert_eq!(
        None,
        first_mismatch(&data, offset),
        "pattern mismatch across the single to double indirect boundary at sector size {sector_size}"
    );
}

generate_tests!(
    read_unaligned_in_double_indirect:
    512 - read_unaligned_in_double_indirect_512,
    1024 - read_unaligned_in_double_indirect_1024,
    4096 - read_unaligned_in_double_indirect_4096,
);

fn read_unaligned_in_double_indirect(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/large.img", sector_size);
    let file = open_pattern(&fs);

    let offset = 500 * 1024 + 333;
    let data = read_exact_at(&fs, &file, offset, 77);
    assert_eq!(
        None,
        first_mismatch(&data, offset),
        "pattern mismatch for the unaligned double indirect read at sector size {sector_size}"
    );
}

generate_tests!(
    read_across_sparse_hole:
    512 - read_across_sparse_hole_512,
    1024 - read_across_sparse_hole_1024,
    4096 - read_across_sparse_hole_4096,
);

fn read_across_sparse_hole(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/large.img", sector_size);
    let root = fs.read_root_inode().expect("root inode must be readable");
    let file: RegularFile = fs
        .find_and_resolve_entry(&root, |e| e.name().is_some_and(|n| n == "sparse.bin"))
        .expect("directory lookup must succeed")
        .expect("sparse.bin must exist in the image")
        .try_into()
        .expect("sparse.bin must be a regular file");
    assert_eq!(3072, file.len(), "unexpected sparse.bin size");

    let data = read_exact_at(&fs, &file, 0, 3072);
    for (i, &b) in data.iter().enumerate() {
        let expected = match i / 1024 {
            0 => 0xAB,
            1 => 0x00,
            _ => 0xEF,
        };
        assert_eq!(
            expected, b,
            "unexpected byte at offset {i} of sparse.bin at sector size {sector_size}"
        );
    }
}

generate_tests!(
    huge_file_length:
    512 - huge_file_length_512,
    1024 - huge_file_length_1024,
    4096 - huge_file_length_4096,
);

fn huge_file_length(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/huge.img", sector_size);
    let file = open_huge(&fs);
    assert_eq!(
        HUGE_LEN,
        file.len(),
        "unexpected huge.bin size at sector size {sector_size}"
    );
}

#[test]
fn indirect_limits_follow_the_1k_block_size() {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/large.img", 512);
    assert_eq!(1024, fs.superblock().block_size());
    assert_eq!(
        IndirectPointerLimits {
            direct_end: 12,
            indirect_end: 12 + 256,
            double_indirect_end: 12 + 256 + 256 * 256,
        },
        fs.indirect_pointer_limits()
    );
}

generate_tests!(
    read_across_triple_indirect_boundary:
    512 - read_across_triple_indirect_boundary_512,
    1024 - read_across_triple_indirect_boundary_1024,
    4096 - read_across_triple_indirect_boundary_4096,
);

fn read_across_triple_indirect_boundary(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/huge.img", sector_size);
    let file = open_huge(&fs);

    let (offset, len) = TRIPLE_BOUNDARY_WINDOW;
    let data = read_exact_at(&fs, &file, offset, len);
    assert_eq!(
        None,
        first_mismatch(&data, offset),
        "pattern mismatch across the double to triple indirect boundary at sector size {sector_size}"
    );
}

generate_tests!(
    read_across_inner_double_indirect_boundary:
    512 - read_across_inner_double_indirect_boundary_512,
    1024 - read_across_inner_double_indirect_boundary_1024,
    4096 - read_across_inner_double_indirect_boundary_4096,
);

fn read_across_inner_double_indirect_boundary(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/huge.img", sector_size);
    let file = open_huge(&fs);

    let (offset, len) = INNER_DOUBLE_WINDOW;
    let data = read_exact_at(&fs, &file, offset, len);
    assert_eq!(
        None,
        first_mismatch(&data, offset),
        "pattern mismatch across the inner double indirect boundary at sector size {sector_size}"
    );
}

generate_tests!(
    read_hole_in_triple_indirect:
    512 - read_hole_in_triple_indirect_512,
    1024 - read_hole_in_triple_indirect_1024,
    4096 - read_hole_in_triple_indirect_4096,
);

fn read_hole_in_triple_indirect(sector_size: usize) {
    let fs = cow_fs!("kernel/ext2/tests/filesystems/huge.img", sector_size);
    let file = open_huge(&fs);

    let offset = 100 * 1024 * 1024;
    let data = read_exact_at(&fs, &file, offset, 1024);
    assert!(
        data.iter().all(|b| *b == 0),
        "hole at offset {offset} must read as zeros at sector size {sector_size}"
    );
}

struct CountingDevice {
    inner: MemoryBlockDevice<Vec<u8>>,
    reads: Rc<Cell<usize>>,
    writes: Rc<Cell<usize>>,
}

impl BlockDevice for CountingDevice {
    type Error = ();

    fn sector_size(&self) -> usize {
        self.inner.sector_size()
    }

    fn sector_count(&self) -> usize {
        self.inner.sector_count()
    }

    fn read_sector(&self, sector_index: usize, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.reads.set(self.reads.get() + 1);
        self.inner.read_sector(sector_index, buf)
    }

    fn write_sector(&mut self, sector_index: usize, buf: &[u8]) -> Result<usize, Self::Error> {
        self.writes.set(self.writes.get() + 1);
        self.inner.write_sector(sector_index, buf)
    }
}

fn counting_fs(image: &str) -> (Ext2Fs<CountingDevice>, Rc<Cell<usize>>, Rc<Cell<usize>>) {
    let image_data = common::load_copy_of_image(image);
    let reads = Rc::new(Cell::new(0));
    let writes = Rc::new(Cell::new(0));
    let device = CountingDevice {
        inner: MemoryBlockDevice::try_new(512, image_data).expect("device creation must succeed"),
        reads: Rc::clone(&reads),
        writes: Rc::clone(&writes),
    };
    let fs = Ext2Fs::try_new(device).expect("filesystem must mount");
    (fs, reads, writes)
}

// Metadata (bitmap, counters, inode, indirect tables) reaches the device at
// sync only, so a sequential write costs its data sectors and little else.
#[test]
fn sequential_write_sector_amplification() {
    const LEN: usize = 512 * 1024;
    let (mut fs, _, writes) = counting_fs("kernel/ext2/tests/filesystems/empty.img");
    let mut root = fs.read_root_inode().expect("root inode must be readable");
    let mut file = fs
        .create_regular_file(&mut root, "seq.bin")
        .expect("file creation must succeed");
    writes.set(0);

    let chunk = vec![0xA5_u8; 4096];
    for offset in (0..LEN).step_by(chunk.len()) {
        assert_eq!(
            fs.write_to_file(&mut file, offset, &chunk)
                .expect("write must succeed"),
            chunk.len()
        );
    }
    let data_sectors = LEN / 512;
    assert_eq!(
        writes.get(),
        data_sectors,
        "writes before sync must be data only"
    );

    fs.sync().expect("sync must succeed");
    let sectors = writes.get();
    // 512 blocks need 2 indirect tables and 1 double indirect table, plus the
    // bitmap, descriptor table, superblock and the inode table blocks of the
    // file and its parent: 8 blocks
    assert!(
        sectors <= data_sectors + 8 * 2,
        "sequential write issued {sectors} write_sector calls, data alone needs {data_sectors}"
    );
}

#[test]
fn sequential_read_sector_amplification() {
    let (fs, reads, _) = counting_fs(IMAGE);
    let file = open_pattern(&fs);

    let mut offset = 0;
    while offset < PATTERN_LEN {
        let len = 4096.min(PATTERN_LEN - offset);
        let data = read_exact_at(&fs, &file, offset, len);
        assert_eq!(
            None,
            first_mismatch(&data, offset),
            "pattern mismatch in the sequential scan at offset {offset}"
        );
        offset += len;
    }

    let sectors = reads.get();
    assert!(
        sectors < 1500,
        "sequential scan issued {sectors} read_sector calls, data alone needs {}",
        PATTERN_LEN.div_ceil(512)
    );
}
