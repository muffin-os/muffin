use crate::api::{GfxCompiler, PipelineDesc};
use crate::error::{GfxError, Result};

/// Input to [`SoftCompiler::compile_shader`]: describes a single shader stage
/// as a Rust function pointer.
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

/// A compiled single-stage shader handle for the software backend.
pub enum SoftShader {
    Vertex {
        func: fn(&[f32], &mut [f32]),
        output_count: usize,
    },
    Fragment(fn(&[f32]) -> u32),
}

/// CPU-interpreted render state produced by the software compiler.
pub struct SoftPipeline {
    pub blend: bool,
    pub depth: bool,
    pub vertex_fn: fn(&[f32], &mut [f32]),
    pub fragment_fn: fn(&[f32]) -> u32,
    /// Bytes between the start of consecutive vertices in the input buffer.
    pub vertex_stride: usize,
    /// Total number of `f32` values the vertex shader writes per vertex
    /// (position counts as 2).
    pub output_count: usize,
}

/// Software compiler; translates shader definitions and pipeline descriptions
/// into CPU-executable render state.
pub struct SoftCompiler;

impl GfxCompiler for SoftCompiler {
    type Pipeline = SoftPipeline;
    type Shader = SoftShader;
    type ShaderSource = SoftShaderDef;

    fn compile_shader(&mut self, source: &SoftShaderDef) -> Result<SoftShader> {
        Ok(match source {
            SoftShaderDef::Vertex { func, output_count } => SoftShader::Vertex {
                func: *func,
                output_count: *output_count,
            },
            SoftShaderDef::Fragment(f) => SoftShader::Fragment(*f),
        })
    }

    fn compile_pipeline<'a>(
        &mut self,
        desc: &PipelineDesc<'a, SoftShader>,
    ) -> Result<SoftPipeline> {
        let (vertex_fn, output_count) = match desc.vertex_shader {
            SoftShader::Vertex { func, output_count } => (*func, *output_count),
            SoftShader::Fragment(_) => return Err(GfxError::WrongShaderKind),
        };
        let fragment_fn = match desc.pixel_shader {
            SoftShader::Fragment(f) => *f,
            SoftShader::Vertex { .. } => return Err(GfxError::WrongShaderKind),
        };
        Ok(SoftPipeline {
            blend: desc.blend,
            depth: desc.depth,
            vertex_fn,
            fragment_fn,
            vertex_stride: desc.vertex_stride,
            output_count,
        })
    }
}
