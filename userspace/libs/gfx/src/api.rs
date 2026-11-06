use kernel_abi::gfx::{BufferDesc, TextureDesc};

use crate::error::Result;

/// A rectangle in pixels, with the origin at the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Bounding box of both rectangles; edges saturate at `u32::MAX`.
    pub fn union(self, other: Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x.saturating_add(self.width)).max(other.x.saturating_add(other.width));
        let bottom = (self.y.saturating_add(self.height)).max(other.y.saturating_add(other.height));
        Rect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }

    /// Returns true when the rectangles share one or more pixels. An empty
    /// rectangle intersects nothing.
    pub fn intersects(self, other: Rect) -> bool {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self
            .x
            .saturating_add(self.width)
            .min(other.x.saturating_add(other.width));
        let bottom = self
            .y
            .saturating_add(self.height)
            .min(other.y.saturating_add(other.height));
        left < right && top < bottom
    }
}

/// The data type of one vertex attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeFormat {
    F32,
    F32x2,
    F32x3,
    F32x4,
}

impl AttributeFormat {
    /// Returns the size of the attribute in bytes.
    #[must_use]
    pub const fn byte_len(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F32x2 => 8,
            Self::F32x3 => 12,
            Self::F32x4 => 16,
        }
    }
}

/// One attribute in a vertex, `offset` bytes from the start of the vertex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexAttribute {
    pub offset: usize,
    pub format: AttributeFormat,
}

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
    /// Bytes between the start of consecutive vertices in the vertex buffer.
    pub vertex_stride: usize,
    /// The layout of one vertex. Attribute `i` feeds shader input `i`.
    pub vertex_attributes: &'a [VertexAttribute],
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
    /// from the currently bound vertex buffer. Fails when no pipeline or
    /// no vertex buffer is bound.
    fn draw(&mut self, vertices: u32) -> Result<()>;
    /// Fills the whole target with `argb` (`0xAARRGGBB`).
    fn clear(&mut self, argb: u32);
    /// Copies `src_rect` of `src` to the target at (`dst_x`, `dst_y`). The copy clips to both images.
    fn blit(&mut self, src: &B::Texture, src_rect: Rect, dst_x: u32, dst_y: u32) -> Result<()>;
}

/// Submits recorded command buffers to the GPU and manages frame
/// presentation.
pub trait GfxQueue<B: GfxBackend> {
    /// The recorder type used to encode commands before submission.
    type Recorder: CommandRecorder<B>;
    /// The fence type that [`GfxQueue::submit`] returns.
    type Fence: GfxFence;

    /// Records a batch of commands via `recorder_cmds` and submits
    /// them to the GPU for execution. Nothing is executed when
    /// `recorder_cmds` fails.
    fn submit<F>(&mut self, recorder_cmds: F) -> Result<Self::Fence>
    where
        F: FnOnce(&mut Self::Recorder) -> Result<()>;

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
    /// Returns the device-wide monotonic id of the fence. `0` means that no GPU work is pending.
    fn id(&self) -> u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn intersects_needs_a_shared_pixel() {
        let cursor = rect(10, 10, 14, 20);
        assert!(cursor.intersects(rect(0, 0, 11, 11)), "corner overlap");
        assert!(cursor.intersects(rect(12, 12, 1, 1)), "inside");
        assert!(!cursor.intersects(rect(0, 0, 10, 10)), "touching corner");
        assert!(!cursor.intersects(rect(24, 10, 5, 5)), "right of it");
        assert!(!cursor.intersects(rect(10, 30, 5, 5)), "below it");
        assert!(!cursor.intersects(rect(12, 12, 0, 5)), "empty");
    }
}
