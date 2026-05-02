use crate::api::Adapter;

/// Software rasterizer adapter; always reports no hardware acceleration.
pub struct SoftAdapter;

impl Adapter for SoftAdapter {
    fn max_texture_resolution(&self) -> u32 {
        4096
    }

    fn is_hardware_accelerated(&self) -> bool {
        false
    }
}
