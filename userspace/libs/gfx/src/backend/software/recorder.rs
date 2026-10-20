use alloc::rc::Rc;
use alloc::vec::Vec;

use super::{SoftBackend, SoftBuffer, SoftPipeline};
use crate::api::CommandRecorder;
use crate::error::{GfxError, Result};

/// A single recorded draw call with all state snapshotted at `draw()` time.
#[derive(Debug)]
pub struct DrawCmd {
    pub pipeline: SoftPipeline,
    /// Raw bytes of the vertex buffer bound at `draw()` time, shared with
    /// every other draw recorded against the same binding.
    pub raw_data: Rc<[u8]>,
    pub vertices: u32,
}

/// Software command recorder; accumulates draw calls for CPU-side execution.
#[derive(Debug, Default)]
pub struct SoftRecorder {
    pipeline: Option<SoftPipeline>,
    raw_data: Option<Rc<[u8]>>,
    pub cmds: Vec<DrawCmd>,
}

impl SoftRecorder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CommandRecorder<SoftBackend> for SoftRecorder {
    fn bind_pipeline(&mut self, pso: &SoftPipeline) {
        self.pipeline = Some(*pso);
    }

    fn bind_vertex_buffer(&mut self, buf: &SoftBuffer) {
        self.raw_data = Some(Rc::from(buf.data.as_slice()));
    }

    fn draw(&mut self, vertices: u32) -> Result<()> {
        let pipeline = self.pipeline.ok_or(GfxError::PipelineNotBound)?;
        let raw_data = self
            .raw_data
            .clone()
            .ok_or(GfxError::VertexBufferNotBound)?;
        self.cmds.push(DrawCmd {
            pipeline,
            raw_data,
            vertices,
        });
        Ok(())
    }
}
