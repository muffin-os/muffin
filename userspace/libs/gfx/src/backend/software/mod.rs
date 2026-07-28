#[cfg(test)]
mod tests;

pub mod adapter;
pub mod allocator;
pub mod compiler;
pub mod fence;
pub mod queue;
pub mod recorder;

pub use adapter::SoftAdapter;
pub use allocator::{SoftAllocator, SoftBuffer, SoftTexture};
pub use compiler::{SoftCompiler, SoftPipeline, SoftShader, SoftShaderDef};
pub use fence::SoftFence;
use kernel_abi::gfx::{BufferDesc, TextureDesc};
pub use queue::SoftQueue;
pub use recorder::{DrawCmd, SoftRecorder};

use crate::api::{GfxAllocator, GfxBackend, GfxCompiler, PipelineDesc};
use crate::error::Result;

/// Software backend combining [`SoftAllocator`] and [`SoftCompiler`].
pub struct SoftBackend(pub SoftAllocator, pub SoftCompiler);

impl GfxAllocator for SoftBackend {
    type Buffer = SoftBuffer;
    type Texture = SoftTexture;

    fn alloc_buffer(&mut self, desc: &BufferDesc) -> Result<SoftBuffer> {
        self.0.alloc_buffer(desc)
    }

    fn alloc_texture(&mut self, desc: &TextureDesc) -> Result<SoftTexture> {
        self.0.alloc_texture(desc)
    }
}

impl GfxCompiler for SoftBackend {
    type Pipeline = SoftPipeline;
    type Shader = SoftShader;
    type ShaderSource = SoftShaderDef;

    fn compile_shader(&mut self, source: &SoftShaderDef) -> Result<SoftShader> {
        self.1.compile_shader(source)
    }

    fn compile_pipeline<'a>(
        &mut self,
        desc: &PipelineDesc<'a, SoftShader>,
    ) -> Result<SoftPipeline> {
        self.1.compile_pipeline(desc)
    }
}

impl GfxBackend for SoftBackend {}
