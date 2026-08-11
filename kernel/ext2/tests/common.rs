use std::fs;
use std::path::Path;

#[macro_export]
macro_rules! cow_fs {
    ($path:literal, $device_sector_size:expr) => {{
        let image_data = common::load_copy_of_image($path);
        let device = MemoryBlockDevice::try_new($device_sector_size, image_data).unwrap();
        Ext2Fs::try_new(device).unwrap()
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
