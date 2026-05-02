use kernel_abi::gfx::{BufferDesc, TextureDesc};

use crate::error::Result;

/// Queries hardware capabilities and limits.
pub trait Adapter {
    /// Returns the maximum allowed dimension (width or height) for a
    /// single texture.
    fn max_texture_resolution(&self) -> u32;
    /// Whether this adapter represents a physical GPU device.
    fn is_hardware_accelerated(&self) -> bool;
}

/// Manages raw memory sub-allocation for graphics resources
/// (VRAM or system RAM).
pub trait GfxAllocator {
    /// The internal driver type representing a linear memory buffer.
    type Buffer;
    /// The internal driver type representing an image/texture.
    type Texture;

    /// Allocates and initializes a linear memory buffer.
    fn alloc_buffer(&mut self, desc: &BufferDesc) -> Result<Self::Buffer>;
    /// Allocates and initializes a 2d texture.
    fn alloc_texture(&mut self, desc: &TextureDesc) -> Result<Self::Texture>;
}

/// Describes the render state and compiled shaders needed for a draw call.
/// `S` is the backend's compiled shader handle type.
pub struct PipelineDesc<'a, S> {
    /// Compiled vertex shader for this pipeline.
    pub vertex_shader: &'a S,
    /// Compiled fragment/pixel shader for this pipeline.
    pub pixel_shader: &'a S,
    /// Whether alpha blending is enabled.
    pub blend: bool,
    /// Whether depth testing and writing are enabled.
    pub depth: bool,
    /// Bytes between the start of consecutive vertices in the vertex buffer.
    pub vertex_stride: usize,
}

/// Compiles shader programs and render state descriptions into
/// driver-specific Pipeline State Objects (PSOs).
pub trait GfxCompiler {
    /// The internal driver type representing a compiled PSO.
    type Pipeline;
    /// The internal driver type representing a compiled single-stage shader.
    type Shader;
    /// The input type consumed by [`compile_shader`]. Use `?Sized` to allow
    /// unsized sources such as `[u8]` for SPIR-V byte slices.
    ///
    /// [`compile_shader`]: GfxCompiler::compile_shader
    type ShaderSource: ?Sized;

    /// Compiles a single shader stage from its source representation.
    fn compile_shader(&mut self, source: &Self::ShaderSource) -> Result<Self::Shader>;

    /// Assembles a pipeline from already-compiled shader handles and render state.
    fn compile_pipeline<'a>(
        &mut self,
        desc: &PipelineDesc<'a, Self::Shader>,
    ) -> Result<Self::Pipeline>;
}

/// Combines memory allocation and pipeline compilation into a single
/// driver backend, required by [`CommandRecorder`] and [`GfxQueue`].
pub trait GfxBackend: GfxAllocator + GfxCompiler {}

/// Encodes draw commands into a command buffer for later submission.
pub trait CommandRecorder<B: GfxBackend> {
    /// Binds a compiled pipeline state object, making it active for
    /// subsequent draw calls.
    fn bind_pipeline(&mut self, pso: &B::Pipeline);
    /// Binds a vertex buffer as the source of per-vertex data for
    /// subsequent draw calls.
    fn bind_vertex_buffer(&mut self, buf: &B::Buffer);
    /// Records a non-indexed draw call consuming `vertices` vertices
    /// from the currently bound vertex buffer.
    fn draw(&mut self, vertices: u32);
}

/// Submits recorded command buffers to the GPU and manages frame
/// presentation.
pub trait GfxQueue<B: GfxBackend> {
    /// The recorder type used to encode commands before submission.
    type Recorder: CommandRecorder<B>;

    /// Records a batch of commands via `recorder_cmds` and submits
    /// them to the GPU for execution.
    fn submit<F>(&mut self, recorder_cmds: F) -> Result<()>
    where
        F: FnOnce(&mut Self::Recorder);

    /// Presents the current frame to the display surface.
    fn present(&mut self) -> Result<()>;
}

/// A synchronization primitive that signals when pending GPU work has
/// completed.
pub trait GfxFence {
    /// Returns `true` if all GPU work associated with this fence has
    /// finished.
    fn is_ready(&self) -> bool;
    /// Blocks the calling thread until all GPU work associated with
    /// this fence has finished.
    fn wait(&self);
}
