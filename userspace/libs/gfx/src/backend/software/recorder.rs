use alloc::vec;
use alloc::vec::Vec;

use super::{SoftBackend, SoftBuffer, SoftPipeline};
use crate::api::CommandRecorder;

/// A single recorded draw call with all state snapshotted at `draw()` time.
#[derive(Debug)]
pub struct DrawCmd {
    pub pipeline: SoftPipeline,
    /// Raw bytes from the bound vertex buffer.
    pub raw_data: Vec<u8>,
    pub vertices: u32,
}

/// Software command recorder; accumulates draw calls for CPU-side execution.
#[derive(Debug)]
pub struct SoftRecorder {
    pipeline: SoftPipeline,
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
            // A zero stride makes the rasterizer skip draws issued before any
            // pipeline is bound.
            pipeline: SoftPipeline {
                vertex_fn: noop_vert,
                fragment_fn: noop_frag,
                vertex_stride: 0,
                output_count: 0,
            },
            raw_data: vec![],
            cmds: vec![],
        }
    }
}

impl CommandRecorder<SoftBackend> for SoftRecorder {
    fn bind_pipeline(&mut self, pso: &SoftPipeline) {
        self.pipeline = *pso;
    }

    fn bind_vertex_buffer(&mut self, buf: &SoftBuffer) {
        self.raw_data = buf.data.clone();
    }

    fn draw(&mut self, vertices: u32) {
        self.cmds.push(DrawCmd {
            pipeline: self.pipeline,
            raw_data: self.raw_data.clone(),
            vertices,
        });
    }
}
