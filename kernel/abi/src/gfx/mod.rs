/// Defines the memory layout and usage pattern for a linear
/// data buffer.
#[repr(C)]
pub struct BufferDesc {
    /// Total size of the buffer in bytes.
    pub size: usize,
    /// Whether the buffer will be written frequently from the CPU.
    /// Set to `false` for geometry that is uploaded once and never changed.
    pub is_dynamic: bool,
}

/// Defines the dimensions and pixel layout for a 2d image resource.
#[repr(C)]
pub struct TextureDesc {
    /// Width of the texture in texels.
    pub width: u32,
    /// Height of the texture in texels.
    pub height: u32,
    /// Pixel format describing the channel layout and bit depth.
    pub format: TextureFormat,
}

/// Defines the immutable render state and shader programs for
/// a draw call.
#[repr(C)]
pub struct PipelineDesc<'a> {
    /// Bytecode or identifier for the vertex shader program.
    pub vertex_shader: &'a [u8],
    /// Bytecode or identifier for the pixel/fragment shader program.
    pub pixel_shader: &'a [u8],
    /// Whether alpha blending is enabled for the output
    /// framebuffer.
    pub blend: bool,
    /// Whether depth testing and writing are enabled.
    pub depth: bool,
}

/// Specifies the color channel layout and bit-depth of a texture.
#[repr(C)]
pub enum TextureFormat {
    /// 32-bit format with 8 bits per channel
    /// in unsigned normalized integer format.
    Rgba8Unorm,
}

/// Represents a single instruction to be encoded into a command
/// buffer.
#[repr(C)]
pub enum CommandOp {
    /// Binds a compiled pipeline state object (PSO) using its
    /// hardware/driver handle.
    BindPipeline(u32),
    /// Triggers a draw execution using the specified number of
    /// vertices.
    Draw(u32),
}
