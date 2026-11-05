use alloc::rc::Rc;
use alloc::vec::Vec;

use super::{SoftBackend, SoftBuffer, SoftPipeline, SoftTexture};
use crate::api::{CommandRecorder, Rect};
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

/// One recorded command.
#[derive(Debug)]
pub enum SoftCmd {
    Draw(DrawCmd),
    /// Fills the target with a `0xAARRGGBB` color.
    Clear(u32),
    /// Copies `src_rect` of `pixels` (rows of `src_width` pixels) to (`dst_x`, `dst_y`).
    Blit {
        pixels: Rc<[u32]>,
        src_width: u32,
        src_rect: Rect,
        dst_x: u32,
        dst_y: u32,
    },
}

/// Software command recorder; accumulates draw calls for CPU-side execution.
#[derive(Debug, Default)]
pub struct SoftRecorder {
    pipeline: Option<SoftPipeline>,
    raw_data: Option<Rc<[u8]>>,
    pub cmds: Vec<SoftCmd>,
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
        self.cmds.push(SoftCmd::Draw(DrawCmd {
            pipeline,
            raw_data,
            vertices,
        }));
        Ok(())
    }

    fn clear(&mut self, argb: u32) {
        self.cmds.push(SoftCmd::Clear(argb));
    }

    fn blit(&mut self, src: &SoftTexture, src_rect: Rect, dst_x: u32, dst_y: u32) -> Result<()> {
        let pixels = src
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_ne_bytes(*bytes))
            .collect();
        self.cmds.push(SoftCmd::Blit {
            pixels,
            src_width: src.width,
            src_rect,
            dst_x,
            dst_y,
        });
        Ok(())
    }
}
