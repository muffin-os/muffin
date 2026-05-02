use alloc::vec;
use alloc::vec::Vec;

use kernel_abi::gfx::{BufferDesc, TextureDesc};

use crate::api::GfxAllocator;
use crate::error::Result;

/// A CPU-side linear memory buffer.
pub struct SoftBuffer {
    pub data: Vec<u8>,
}

/// A CPU-side 2D image resource.
pub struct SoftTexture {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Software allocator; manages CPU-side memory for buffers and textures.
pub struct SoftAllocator;

impl GfxAllocator for SoftAllocator {
    type Buffer = SoftBuffer;
    type Texture = SoftTexture;

    fn alloc_buffer(&mut self, desc: &BufferDesc) -> Result<SoftBuffer> {
        Ok(SoftBuffer {
            data: vec![0u8; desc.size],
        })
    }

    fn alloc_texture(&mut self, desc: &TextureDesc) -> Result<SoftTexture> {
        // 4 bytes per pixel for Rgba8Unorm (the only current format)
        let len = desc.width as usize * desc.height as usize * 4;
        Ok(SoftTexture {
            data: vec![0u8; len],
            width: desc.width,
            height: desc.height,
        })
    }
}
