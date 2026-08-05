use alloc::vec::Vec;

use super::{DrawCmd, SoftBackend, SoftRecorder};
use crate::api::GfxQueue;
use crate::error::Result;

/// Rasterizes into a caller-owned framebuffer slice borrowed for `'fb`. The
/// queue never owns or allocates the backing pixels.
///
/// A steady state frame performs no allocation.
pub struct SoftQueue<'fb> {
    target: &'fb mut [u32],
    width: u32,
    height: u32,
    stride: usize,
    vertex_input: Vec<f32>,
    vertex_outputs: Vec<f32>,
    interpolants: Vec<f32>,
    deltas: Vec<f32>,
}

/// Returns the first `len` floats of `buf`, zeroed, growing `buf` if needed.
///
/// The zeroing is load-bearing. A vertex shader that writes only some of its
/// outputs would otherwise observe the previous draw's values, since `buf`
/// outlives the draw.
fn scratch(buf: &mut Vec<f32>, len: usize) -> &mut [f32] {
    if buf.len() < len {
        buf.resize(len, 0.0);
    }
    let out = &mut buf[..len];
    out.fill(0.0);
    out
}

impl<'fb> SoftQueue<'fb> {
    /// Rasterizes into `target`, which must hold at least
    /// `stride * (height - 1) + width` u32 pixels. `stride` is measured in
    /// u32 pixels, not bytes, so a caller holding a byte pitch passes
    /// `pitch / 4`.
    ///
    /// Never clears `target`. Uncovered pixels keep whatever was already
    /// there, so the caller must clear once before the first draw.
    pub fn new(target: &'fb mut [u32], width: u32, height: u32, stride: usize) -> Self {
        let w = width as usize;
        let h = height as usize;
        assert!(stride >= w, "stride {stride} smaller than width {width}");
        assert!(
            target.len() >= stride * (h - 1) + w,
            "target len {} too small for {width}x{height} at stride {stride}",
            target.len()
        );
        Self {
            target,
            width,
            height,
            stride,
            vertex_input: Vec::new(),
            vertex_outputs: Vec::new(),
            interpolants: Vec::new(),
            deltas: Vec::new(),
        }
    }

    /// Returns the target as a flat slice of `0xAARRGGBB` pixels, row-major
    /// from top-left. Rows are `stride` pixels apart, so a target with
    /// `stride > width` carries padding columns between rows.
    pub fn framebuffer(&self) -> &[u32] {
        &*self.target
    }

    fn rasterize(&mut self, cmd: &DrawCmd) {
        let vertex_stride = cmd.vertex_stride;
        if vertex_stride == 0 || cmd.output_count < 2 {
            return;
        }

        let w = self.width as f32;
        let h = self.height as f32;
        let target_stride = self.stride;
        let Self {
            target,
            vertex_input,
            vertex_outputs,
            interpolants,
            deltas,
            ..
        } = self;

        let vert_count = cmd.vertices as usize;
        let output_count = cmd.output_count;
        let interp_count = output_count - 2;
        let outputs = scratch(vertex_outputs, vert_count * output_count);
        let input = scratch(vertex_input, vertex_stride / 4);
        let mut filled = 0usize;
        for i in 0..vert_count {
            let start = i * vertex_stride;
            let end = start + vertex_stride;
            if end > cmd.raw_data.len() {
                break;
            }
            for (dst, chunk) in input
                .iter_mut()
                .zip(cmd.raw_data[start..end].as_chunks::<4>().0)
            {
                *dst = f32::from_ne_bytes(*chunk);
            }
            (cmd.vertex_fn)(
                input,
                &mut outputs[i * output_count..(i + 1) * output_count],
            );
            filled += 1;
        }

        let to_screen =
            |x: f32, y: f32| -> (f32, f32) { ((x + 1.0) * 0.5 * w, (1.0 - y) * 0.5 * h) };

        let interpolants = scratch(interpolants, interp_count);
        let deltas = scratch(deltas, interp_count);

        // Only a triangle list is supported. Strip or fan input silently
        // rasterizes the wrong geometry.
        let tri_count = filled / 3;
        for t in 0..tri_count {
            let v0 = &outputs[t * 3 * output_count..(t * 3 + 1) * output_count];
            let v1 = &outputs[(t * 3 + 1) * output_count..(t * 3 + 2) * output_count];
            let v2 = &outputs[(t * 3 + 2) * output_count..(t * 3 + 3) * output_count];

            let (x0, y0) = to_screen(v0[0], v0[1]);
            let (x1, y1) = to_screen(v1[0], v1[1]);
            let (x2, y2) = to_screen(v2[0], v2[1]);

            // Signed, so the weights it scales stay correct for both CW and CCW
            // winding and orientation never needs a separate test.
            let total_area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
            if total_area.abs() < 1e-6 {
                continue; // near-zero area would make inv_area below diverge
            }
            let inv_area = 1.0 / total_area;

            let min_x = x0.min(x1).min(x2).max(0.0) as u32;
            let min_y = y0.min(y1).min(y2).max(0.0) as u32;
            let max_x = (x0.max(x1).max(x2) as u32).min(self.width.saturating_sub(1));
            let max_y = (y0.max(y1).max(y2) as u32).min(self.height.saturating_sub(1));

            // Barycentric edge functions are affine in (cx, cy), so each is
            // fully determined by its value at one point plus a constant
            // per-pixel step.
            let w0_dx = (y1 - y2) * inv_area;
            let w1_dx = (y2 - y0) * inv_area;
            let w2_dx = -(w0_dx + w1_dx);
            for k in 0..interp_count {
                deltas[k] = w0_dx * v0[2 + k] + w1_dx * v1[2 + k] + w2_dx * v2[2 + k];
            }

            let cx0 = min_x as f32 + 0.5;
            for py in min_y..=max_y {
                let cy = py as f32 + 0.5;
                let w0_cx0 = ((x1 - cx0) * (y2 - cy) - (x2 - cx0) * (y1 - cy)) * inv_area;
                let w1_cx0 = ((cx0 - x0) * (y2 - y0) - (x2 - x0) * (cy - y0)) * inv_area;
                let w2_cx0 = 1.0 - w0_cx0 - w1_cx0;

                // wk is affine in px, so each edge either tightens one bound of
                // the covered span or excludes the whole row.
                let mut lo = min_x as f32;
                let mut hi = max_x as f32;
                let mut row_excluded = false;
                for (wk_cx0, wk_dx) in [(w0_cx0, w0_dx), (w1_cx0, w1_dx), (w2_cx0, w2_dx)] {
                    if wk_dx > 0.0 {
                        lo = lo.max(min_x as f32 - wk_cx0 / wk_dx);
                    } else if wk_dx < 0.0 {
                        hi = hi.min(min_x as f32 - wk_cx0 / wk_dx);
                    } else if wk_cx0 < 0.0 {
                        row_excluded = true;
                        break;
                    }
                }
                // `hi` can go negative when an edge excludes the row. The
                // check below runs before any cast to u32, because casting a
                // negative f32 to u32 saturates to 0 and would falsely mark
                // pixel 0 as covered. `lo` only ever rises from `min_x`, so it
                // stays non-negative, and `hi >= lo` above therefore
                // guarantees `hi` is too.
                if row_excluded || hi < lo {
                    continue;
                }

                // No floor or ceil exists in no_std. Truncation toward zero
                // equals floor for non-negative inputs, and ceil equals floor
                // plus one unless the value is already integral.
                let lo_floor = lo as u32;
                let x_start = if lo > lo_floor as f32 {
                    lo_floor + 1
                } else {
                    lo_floor
                };
                let x_end = hi as u32;
                if x_start > x_end {
                    continue; // no pixel center falls inside the triangle on this row
                }

                let steps = (x_start - min_x) as f32;
                let w0 = w0_cx0 + steps * w0_dx;
                let w1 = w1_cx0 + steps * w1_dx;
                let w2 = w2_cx0 + steps * w2_dx;
                for k in 0..interp_count {
                    interpolants[k] = w0 * v0[2 + k] + w1 * v1[2 + k] + w2 * v2[2 + k];
                }

                let row_start = py as usize * target_stride;
                let row = &mut target[row_start + x_start as usize..=row_start + x_end as usize];

                // Keep this loop free of iterator adapters. Each one is an
                // out-of-line call per component per pixel in an unoptimised
                // build, which dominates the frame.
                let frag = cmd.fragment_fn;
                let pixels = row.len();
                let mut i = 0usize;
                while i < pixels {
                    row[i] = frag(interpolants);
                    let mut k = 0usize;
                    while k < interp_count {
                        interpolants[k] += deltas[k];
                        k += 1;
                    }
                    i += 1;
                }
            }
        }
    }
}

impl<'fb> GfxQueue<SoftBackend> for SoftQueue<'fb> {
    type Recorder = SoftRecorder;

    fn submit<F>(&mut self, recorder_cmds: F) -> Result<()>
    where
        F: FnOnce(&mut Self::Recorder),
    {
        let mut rec = SoftRecorder::new();
        recorder_cmds(&mut rec);
        for cmd in &rec.cmds {
            self.rasterize(cmd);
        }
        Ok(())
    }

    fn present(&mut self) -> Result<()> {
        Ok(())
    }
}
