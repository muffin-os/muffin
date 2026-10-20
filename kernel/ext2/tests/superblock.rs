use kernel_device::block::MemoryBlockDevice;
use kernel_ext2::{EXT2_MAGIC, Error, Ext2Fs};

const MAGIC_OFFSET: usize = 1024 + 56;
const LOG2_BLOCK_SIZE_OFFSET: usize = 1024 + 24;
const BLOCKS_PER_GROUP_OFFSET: usize = 1024 + 32;
const INODES_PER_GROUP_OFFSET: usize = 1024 + 40;
const NUM_BLOCKS_OFFSET: usize = 1024 + 4;

fn mount(image: Vec<u8>) -> Result<Ext2Fs<MemoryBlockDevice<Vec<u8>>>, Error> {
    Ext2Fs::try_new(MemoryBlockDevice::try_new(512, image).unwrap())
}

fn minimal_image() -> Vec<u8> {
    let mut image = vec![0_u8; 8192];
    image[MAGIC_OFFSET..MAGIC_OFFSET + 2].copy_from_slice(&EXT2_MAGIC.to_le_bytes());
    image[BLOCKS_PER_GROUP_OFFSET..BLOCKS_PER_GROUP_OFFSET + 4]
        .copy_from_slice(&8192_u32.to_le_bytes());
    image[INODES_PER_GROUP_OFFSET..INODES_PER_GROUP_OFFSET + 4]
        .copy_from_slice(&128_u32.to_le_bytes());
    image
}

#[test]
fn test_minimal_image_mounts() {
    assert!(mount(minimal_image()).is_ok());
}

#[test]
fn test_zeroed_device_is_rejected() {
    assert_eq!(
        Err(Error::InvalidSuperblock),
        mount(vec![0_u8; 8192]).map(|_| ())
    );
}

#[test]
fn test_zero_blocks_per_group_is_rejected() {
    let mut image = minimal_image();
    image[BLOCKS_PER_GROUP_OFFSET..BLOCKS_PER_GROUP_OFFSET + 4].fill(0);
    assert_eq!(Err(Error::InvalidSuperblock), mount(image).map(|_| ()));
}

#[test]
fn test_zero_inodes_per_group_is_rejected() {
    let mut image = minimal_image();
    image[INODES_PER_GROUP_OFFSET..INODES_PER_GROUP_OFFSET + 4].fill(0);
    assert_eq!(Err(Error::InvalidSuperblock), mount(image).map(|_| ()));
}

#[test]
fn test_oversized_log2_block_size_is_rejected() {
    let mut image = minimal_image();
    image[LOG2_BLOCK_SIZE_OFFSET] = 7;
    assert_eq!(Err(Error::InvalidSuperblock), mount(image).map(|_| ()));
}

#[test]
fn test_bgdt_spanning_multiple_blocks_mounts() {
    let mut image = minimal_image();
    image[NUM_BLOCKS_OFFSET..NUM_BLOCKS_OFFSET + 4].copy_from_slice(&(33 * 8192_u32).to_le_bytes());
    assert!(mount(image).is_ok());
}
