use alloc::vec;
use alloc::vec::Vec;

use log::{debug, trace};

use super::{DrawCmd, SoftBackend, SoftRecorder};
use crate::api::GfxQueue;
use crate::error::Result;

/// Software queue; executes recorded commands on the CPU and writes
/// pixels to an in-memory framebuffer.
pub struct SoftQueue {
    framebuffer: Vec<u32>,
    width: u32,
    height: u32,
}

impl SoftQueue {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            framebuffer: vec![0u32; width as usize * height as usize],
            width,
            height,
        }
    }

    /// Returns the framebuffer as a flat slice of `0xAARRGGBB` pixels,
    /// row-major from top-left.
    pub fn framebuffer(&self) -> &[u32] {
        &self.framebuffer
    }

    fn rasterize(&mut self, cmd: &DrawCmd) {
        let stride = cmd.vertex_stride;
        if stride == 0 || cmd.output_count < 2 {
            return;
        }

        // Phase 1: run the vertex shader once per vertex, collecting outputs.
        let vert_count = cmd.vertices as usize;
        let mut outputs: Vec<Vec<f32>> = Vec::with_capacity(vert_count);
        for i in 0..vert_count {
            let start = i * stride;
            let end = start + stride;
            if end > cmd.raw_data.len() {
                break;
            }
            let input: Vec<f32> = cmd.raw_data[start..end]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_ne_bytes(*b))
                .collect();
            let mut output = vec![0f32; cmd.output_count];
            (cmd.vertex_fn)(&input, &mut output);
            outputs.push(output);
        }

        let w = self.width as f32;
        let h = self.height as f32;
        let to_screen =
            |x: f32, y: f32| -> (f32, f32) { ((x + 1.0) * 0.5 * w, (1.0 - y) * 0.5 * h) };

        let interp_count = cmd.output_count.saturating_sub(2);

        // Phase 2: rasterize as a triangle list; every 3 outputs form one triangle.
        let tri_count = outputs.len() / 3;
        for t in 0..tri_count {
            let v0 = &outputs[t * 3];
            let v1 = &outputs[t * 3 + 1];
            let v2 = &outputs[t * 3 + 2];

            let (x0, y0) = to_screen(v0[0], v0[1]);
            let (x1, y1) = to_screen(v1[0], v1[1]);
            let (x2, y2) = to_screen(v2[0], v2[1]);

            // Signed area of the triangle; used to normalize barycentric weights.
            // Works for both CW and CCW winding.
            let total_area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
            if total_area.abs() < 1e-6 {
                continue; // degenerate triangle
            }

            let min_x = x0.min(x1).min(x2).max(0.0) as u32;
            let min_y = y0.min(y1).min(y2).max(0.0) as u32;
            let max_x = (x0.max(x1).max(x2) as u32).min(self.width.saturating_sub(1));
            let max_y = (y0.max(y1).max(y2) as u32).min(self.height.saturating_sub(1));

            for py in min_y..=max_y {
                for px in min_x..=max_x {
                    let cx = px as f32 + 0.5;
                    let cy = py as f32 + 0.5;

                    // Barycentric weights via sub-triangle signed areas.
                    let w0 = ((x1 - cx) * (y2 - cy) - (x2 - cx) * (y1 - cy)) / total_area;
                    let w1 = ((cx - x0) * (y2 - y0) - (x2 - x0) * (cy - y0)) / total_area;
                    let w2 = 1.0 - w0 - w1;

                    if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                        let mut interpolants = vec![0f32; interp_count];
                        for k in 0..interp_count {
                            interpolants[k] = w0 * v0[2 + k] + w1 * v1[2 + k] + w2 * v2[2 + k];
                        }
                        let color = (cmd.fragment_fn)(&interpolants);
                        let idx = py as usize * self.width as usize + px as usize;
                        self.framebuffer[idx] = color;
                    }
                }
            }
        }
    }
}

impl GfxQueue<SoftBackend> for SoftQueue {
    type Recorder = SoftRecorder;

    fn submit<F>(&mut self, recorder_cmds: F) -> Result<()>
    where
        F: FnOnce(&mut Self::Recorder),
    {
        let mut rec = SoftRecorder::new();
        recorder_cmds(&mut rec);
        debug!("drawing...");
        for cmd in &rec.cmds {
            trace!("drawing {cmd:?}");
            self.rasterize(cmd);
        }
        debug!("done drawing");
        Ok(())
    }

    fn present(&mut self) -> Result<()> {
        Ok(())
    }
}
