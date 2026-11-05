//! The virgl backend over `/dev/gpu0`.
//!
//! Resources live in `/dev/shm` objects that the process maps and imports
//! through the device. Commands are virgl streams; one `write()` submits a
//! stream and returns a fence id.
//!
//! [`encode`] and [`tgsi`] have no syscalls and compile on every target. The
//! types that talk to `/dev/gpu0` live in `device` and exist only for muffin.

pub mod encode;
pub mod tgsi;

#[cfg(target_os = "muffin")]
mod device;
#[cfg(test)]
mod tests;

use alloc::string::String;

#[cfg(target_os = "muffin")]
pub use device::{
    VirtioBackend, VirtioBuffer, VirtioDevice, VirtioFence, VirtioQueue, VirtioRecorder,
    VirtioTexture,
};
pub use encode::{ObjectType, ShaderKind, Stream};

use crate::api::{Rect, VertexAttribute};
use crate::error::{GfxError, Result};

/// The sign of the y scale in the viewport. `-1.0` maps clip space `y = 1`
/// to the first row of the target, so the image in memory has its top at
/// row 0.
pub const VIEWPORT_Y_SIGN: f32 = -1.0;

/// A TGSI shader text with a known stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioShader {
    kind: ShaderKind,
    text: String,
}

impl VirtioShader {
    /// Reads the stage from the first line of `text`.
    pub fn parse(text: &str) -> Result<Self> {
        let kind = if text.starts_with("VERT\n") {
            ShaderKind::Vertex
        } else if text.starts_with("FRAG\n") {
            ShaderKind::Fragment
        } else {
            return Err(GfxError::InvalidShaderSource);
        };
        Ok(Self {
            kind,
            text: String::from(text),
        })
    }

    #[must_use]
    pub fn kind(&self) -> ShaderKind {
        self.kind
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// The virgl object handles of one pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioPipeline {
    vs: u32,
    fs: u32,
    vertex_elements: u32,
    blend: u32,
    rasterizer: u32,
    dsa: u32,
    /// Bytes between the start of consecutive vertices.
    stride: usize,
}

impl VirtioPipeline {
    /// Validates the vertex layout and encodes the creation of every object.
    /// `handles` gives the six object handles in field order.
    pub fn encode(
        stream: &mut Stream,
        handles: [u32; 6],
        vertex: &VirtioShader,
        fragment: &VirtioShader,
        stride: usize,
        attributes: &[VertexAttribute],
    ) -> Result<Self> {
        if vertex.kind != ShaderKind::Vertex || fragment.kind != ShaderKind::Fragment {
            return Err(GfxError::WrongShaderKind);
        }
        if stride == 0 || !stride.is_multiple_of(size_of::<f32>()) {
            return Err(GfxError::InvalidVertexStride);
        }
        for attribute in attributes {
            let end = attribute.offset.checked_add(attribute.format.byte_len());
            if end.is_none_or(|end| end > stride) {
                return Err(GfxError::InvalidVertexAttribute);
            }
        }
        let [vs, fs, vertex_elements, blend, rasterizer, dsa] = handles;
        stream.create_shader(vs, ShaderKind::Vertex, &vertex.text)?;
        stream.create_shader(fs, ShaderKind::Fragment, &fragment.text)?;
        stream.create_vertex_elements(vertex_elements, attributes);
        stream.create_blend(blend);
        stream.create_rasterizer(rasterizer);
        stream.create_dsa(dsa);
        Ok(Self {
            vs,
            fs,
            vertex_elements,
            blend,
            rasterizer,
            dsa,
            stride,
        })
    }

    /// Encodes the bind commands for every object of the pipeline.
    pub fn bind(&self, stream: &mut Stream) {
        stream.bind_object(ObjectType::Blend, self.blend);
        stream.bind_object(ObjectType::Rasterizer, self.rasterizer);
        stream.bind_object(ObjectType::Dsa, self.dsa);
        stream.bind_object(ObjectType::VertexElements, self.vertex_elements);
        stream.bind_shader(self.vs, ShaderKind::Vertex);
        stream.bind_shader(self.fs, ShaderKind::Fragment);
    }

    #[must_use]
    pub fn stride(&self) -> usize {
        self.stride
    }
}

/// Clips a copy of `src_rect` from a `src_width` by `src_height` image to
/// (`dst_x`, `dst_y`) in a `dst_width` by `dst_height` image. Returns the
/// clipped source and destination rectangles, or `None` for an empty copy.
#[must_use]
pub fn clip_blit(
    src_width: u32,
    src_height: u32,
    src_rect: Rect,
    dst_x: u32,
    dst_y: u32,
    dst_width: u32,
    dst_height: u32,
) -> Option<(Rect, Rect)> {
    let width = src_rect
        .width
        .min(src_width.saturating_sub(src_rect.x))
        .min(dst_width.saturating_sub(dst_x));
    let height = src_rect
        .height
        .min(src_height.saturating_sub(src_rect.y))
        .min(dst_height.saturating_sub(dst_y));
    if width == 0 || height == 0 {
        return None;
    }
    let src = Rect {
        x: src_rect.x,
        y: src_rect.y,
        width,
        height,
    };
    let dst = Rect {
        x: dst_x,
        y: dst_y,
        width,
        height,
    };
    Some((src, dst))
}
