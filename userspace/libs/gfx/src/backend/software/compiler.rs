use super::ATTRIBUTE_BYTES;
use crate::api::{GfxCompiler, PipelineDesc};
use crate::error::{GfxError, Result};

/// A single shader stage as a Rust function pointer. The software backend
/// runs it directly, so [`SoftCompiler::compile_shader`] returns its input
/// unchanged.
#[derive(Debug, Clone, Copy)]
pub enum SoftShaderDef {
    /// A vertex shader. Reads one vertex worth of `f32` values from `input`
    /// and writes NDC position into `output[0..2]` plus any interpolants into
    /// `output[2..]`. `output_count` is the total length of the output slice.
    Vertex {
        func: fn(input: &[f32], output: &mut [f32]),
        output_count: usize,
    },
    /// A fragment shader. Receives barycentric-interpolated values from the
    /// vertex shader's output and returns a packed `0xAARRGGBB` pixel color.
    Fragment(fn(interpolants: &[f32]) -> u32),
}

/// CPU-interpreted render state produced by the software compiler. Only
/// [`SoftCompiler::compile_pipeline`] builds one, so the rasterizer can rely
/// on the stride and output count it validated.
#[derive(Debug, Clone, Copy)]
pub struct SoftPipeline {
    pub(super) vertex_fn: fn(&[f32], &mut [f32]),
    pub(super) fragment_fn: fn(&[f32]) -> u32,
    /// Bytes between the start of consecutive vertices in the input buffer.
    pub(super) vertex_stride: usize,
    /// Total number of `f32` values the vertex shader writes per vertex
    /// (position counts as 2).
    pub(super) output_count: usize,
}

/// Software compiler; translates shader definitions and pipeline descriptions
/// into CPU-executable render state.
pub struct SoftCompiler;

impl GfxCompiler for SoftCompiler {
    type Pipeline = SoftPipeline;
    type Shader = SoftShaderDef;
    type ShaderSource = SoftShaderDef;

    fn compile_shader(&mut self, source: &SoftShaderDef) -> Result<SoftShaderDef> {
        Ok(*source)
    }

    fn compile_pipeline<'a>(
        &mut self,
        desc: &PipelineDesc<'a, SoftShaderDef>,
    ) -> Result<SoftPipeline> {
        let (vertex_fn, output_count) = match desc.vertex_shader {
            SoftShaderDef::Vertex { func, output_count } => (*func, *output_count),
            SoftShaderDef::Fragment(_) => return Err(GfxError::WrongShaderKind),
        };
        let fragment_fn = match desc.pixel_shader {
            SoftShaderDef::Fragment(f) => *f,
            SoftShaderDef::Vertex { .. } => return Err(GfxError::WrongShaderKind),
        };
        if desc.vertex_stride == 0 || !desc.vertex_stride.is_multiple_of(ATTRIBUTE_BYTES) {
            return Err(GfxError::InvalidVertexStride);
        }
        if output_count < 2 {
            return Err(GfxError::InvalidOutputCount);
        }
        Ok(SoftPipeline {
            vertex_fn,
            fragment_fn,
            vertex_stride: desc.vertex_stride,
            output_count,
        })
    }
}
