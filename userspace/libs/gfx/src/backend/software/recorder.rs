use alloc::vec;
use alloc::vec::Vec;

use super::{SoftBackend, SoftBuffer, SoftPipeline};
use crate::api::CommandRecorder;

/// A single recorded draw call with all state snapshotted at `draw()` time.
#[derive(Debug)]
pub struct DrawCmd {
    pub blend: bool,
    pub depth: bool,
    /// Raw bytes from the bound vertex buffer.
    pub raw_data: Vec<u8>,
    pub vertex_stride: usize,
    pub vertex_fn: fn(&[f32], &mut [f32]),
    pub fragment_fn: fn(&[f32]) -> u32,
    pub output_count: usize,
    pub vertices: u32,
}

/// Software command recorder; accumulates draw calls for CPU-side execution.
#[derive(Debug)]
pub struct SoftRecorder {
    blend: bool,
    depth: bool,
    vertex_fn: fn(&[f32], &mut [f32]),
    fragment_fn: fn(&[f32]) -> u32,
    vertex_stride: usize,
    output_count: usize,
    raw_data: Vec<u8>,
    pub cmds: Vec<DrawCmd>,
}

fn noop_vert(_: &[f32], _: &mut [f32]) {}
fn noop_frag(_: &[f32]) -> u32 {
    0
}

impl SoftRecorder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for SoftRecorder {
    fn default() -> Self {
        Self {
            blend: false,
            depth: false,
            vertex_fn: noop_vert,
            fragment_fn: noop_frag,
            vertex_stride: 0,
            output_count: 0,
            raw_data: vec![],
            cmds: vec![],
        }
    }
}

impl CommandRecorder<SoftBackend> for SoftRecorder {
    fn bind_pipeline(&mut self, pso: &SoftPipeline) {
        self.blend = pso.blend;
        self.depth = pso.depth;
        self.vertex_fn = pso.vertex_fn;
        self.fragment_fn = pso.fragment_fn;
        self.vertex_stride = pso.vertex_stride;
        self.output_count = pso.output_count;
    }

    fn bind_vertex_buffer(&mut self, buf: &SoftBuffer) {
        self.raw_data = buf.data.clone();
    }

    fn draw(&mut self, vertices: u32) {
        self.cmds.push(DrawCmd {
            blend: self.blend,
            depth: self.depth,
            raw_data: self.raw_data.clone(),
            vertex_stride: self.vertex_stride,
            vertex_fn: self.vertex_fn,
            fragment_fn: self.fragment_fn,
            output_count: self.output_count,
            vertices,
        });
    }
}
