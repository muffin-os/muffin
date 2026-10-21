use std::fs;
use std::path::Path;

#[macro_export]
macro_rules! cow_fs {
    ($path:expr, $device_sector_size:expr) => {{
        let image_data = common::load_copy_of_image($path);
        let device = MemoryBlockDevice::try_new($device_sector_size, image_data).unwrap();
        Ext2Fs::try_new(device).unwrap()
    }};
}

/// Copy of the empty fixture whose ext2 block size is `$block_size` bytes.
#[macro_export]
macro_rules! empty_fs {
    ($block_size:expr) => {{
        let fs = cow_fs!(
            match $block_size {
                1024 => "kernel/ext2/tests/filesystems/empty.img",
                4096 => "kernel/ext2/tests/filesystems/empty4k.img",
                other => panic!("no empty fixture with {other} byte blocks"),
            },
            512
        );
        assert_eq!($block_size, fs.superblock().block_size() as usize);
        fs
    }};
}

#[macro_export]
macro_rules! generate_tests {
    ($test_fn:ident : $($size:literal - $name:ident),*,) => {
        const _: &dyn Fn(usize) = &$test_fn;
        $(
            #[test]
            fn $name() {
                $test_fn($size);
            }
        )*
    };
}

pub fn load_copy_of_image(test_image: impl AsRef<Path>) -> Vec<u8> {
    fs::read(test_image).unwrap()
}
