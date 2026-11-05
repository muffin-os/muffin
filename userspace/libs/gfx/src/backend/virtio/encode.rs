//! Encodes virgl command streams.
//!
//! Command numbers, object types and dword layouts come from
//! `virgl_protocol.h` of virglrenderer. Format numbers come from `virgl_hw.h`.
//! Gallium enum values come from `p_defines.h` of virglrenderer.

use alloc::vec::Vec;

use crate::api::{AttributeFormat, Rect, VertexAttribute};
use crate::error::{GfxError, Result};

/// `VIRGL_CMD0_MAX_DWORDS`: the largest dword count that one command header can carry.
pub const CMD0_MAX_DWORDS: usize = ((1 << 16) - 1) / 4 * 4;

/// `enum virgl_context_cmd`.
const CCMD_CREATE_OBJECT: u32 = 1;
const CCMD_BIND_OBJECT: u32 = 2;
const CCMD_SET_VIEWPORT_STATE: u32 = 4;
const CCMD_SET_FRAMEBUFFER_STATE: u32 = 5;
const CCMD_SET_VERTEX_BUFFERS: u32 = 6;
const CCMD_CLEAR: u32 = 7;
const CCMD_DRAW_VBO: u32 = 8;
const CCMD_BLIT: u32 = 16;
const CCMD_BIND_SHADER: u32 = 31;

/// Dwords after the header of a shader object with no stream outputs
/// (`VIRGL_OBJ_SHADER_HDR_SIZE(0)`).
const SHADER_HEADER_DWORDS: usize = 5;

/// `VIRGL_FORMAT_B8G8R8A8_UNORM`, the format of every texture and surface.
const FORMAT_B8G8R8A8_UNORM: u32 = 1;
/// `VIRGL_FORMAT_R32_FLOAT`; the `R32G32`, `R32G32B32` and `R32G32B32A32` formats follow in order.
const FORMAT_R32_FLOAT: u32 = 28;

/// Bits of `S0` in a rasterizer object. Cull face and fill mode stay zero:
/// the device culls no face and fills polygons.
const RS_DEPTH_CLIP: u32 = 1 << 1;
const RS_FRONT_CCW: u32 = 1 << 15;
const RS_HALF_PIXEL_CENTER: u32 = 1 << 29;
const RS_BOTTOM_EDGE_RULE: u32 = 1 << 30;

/// `VIRGL_OBJ_BLEND_S2_RT_COLORMASK(PIPE_MASK_RGBA)`.
const BLEND_COLORMASK_RGBA: u32 = 0xF << 27;
/// `PIPE_CLEAR_COLOR0`.
const CLEAR_COLOR0: u32 = 1 << 2;
/// `PIPE_PRIM_TRIANGLES`.
const PRIM_TRIANGLES: u32 = 4;
/// `VIRGL_CMD_BLIT_S0`: RGBA mask, nearest filter, no scissor, no blend.
const BLIT_S0_RGBA_NEAREST: u32 = 0xF;

/// A virgl object type (`enum virgl_object_type`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Blend = 1,
    Rasterizer = 2,
    Dsa = 3,
    Shader = 4,
    VertexElements = 5,
    Surface = 8,
}

/// A shader stage (`enum pipe_shader_type`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderKind {
    Vertex = 0,
    Fragment = 1,
}

fn attribute_format(format: AttributeFormat) -> u32 {
    match format {
        AttributeFormat::F32 => FORMAT_R32_FLOAT,
        AttributeFormat::F32x2 => FORMAT_R32_FLOAT + 1,
        AttributeFormat::F32x3 => FORMAT_R32_FLOAT + 2,
        AttributeFormat::F32x4 => FORMAT_R32_FLOAT + 3,
    }
}

/// A virgl command stream in native-endian dwords.
#[derive(Debug, Default, Clone)]
pub struct Stream {
    words: Vec<u32>,
}

impl Stream {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the stream as the bytes that the device reads.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        // Safety: a `u32` slice is valid for reads as bytes over its whole length.
        unsafe {
            core::slice::from_raw_parts(
                self.words.as_ptr().cast::<u8>(),
                self.words.len() * size_of::<u32>(),
            )
        }
    }

    /// Returns the encoded dwords.
    #[must_use]
    pub fn words(&self) -> &[u32] {
        &self.words
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// `VIRGL_CMD0`: `len` counts the dwords after the header.
    fn header(&mut self, cmd: u32, obj: u32, len: usize) {
        debug_assert!(len <= CMD0_MAX_DWORDS);
        self.words.push(cmd | (obj << 8) | ((len as u32) << 16));
    }

    fn push(&mut self, word: u32) {
        self.words.push(word);
    }

    fn push_f32(&mut self, value: f32) {
        self.words.push(value.to_bits());
    }

    /// Creates a shader object from TGSI text. The text must fit one command.
    pub fn create_shader(&mut self, handle: u32, kind: ShaderKind, text: &str) -> Result<()> {
        let bytes = text.as_bytes();
        // The device reads the text and its NUL.
        let offlen = bytes.len() + 1;
        let text_dwords = offlen.div_ceil(size_of::<u32>());
        let len = SHADER_HEADER_DWORDS + text_dwords;
        if len > CMD0_MAX_DWORDS {
            return Err(GfxError::InvalidShaderSource);
        }
        self.header(CCMD_CREATE_OBJECT, ObjectType::Shader as u32, len);
        self.push(handle);
        self.push(kind as u32);
        self.push(offlen as u32);
        // virglrenderer sizes its token array from this count. Every token
        // needs at least one byte of text, so the text length is an upper bound.
        self.push(bytes.len() as u32);
        self.push(0);
        for dword in 0..text_dwords {
            let mut word = [0u8; 4];
            for (slot, byte) in word.iter_mut().zip(bytes.iter().skip(dword * 4)) {
                *slot = *byte;
            }
            self.push(u32::from_ne_bytes(word));
        }
        Ok(())
    }

    /// Creates a vertex elements object. Attribute `i` feeds shader input `i`
    /// from vertex buffer 0.
    pub fn create_vertex_elements(&mut self, handle: u32, attrs: &[VertexAttribute]) {
        self.header(
            CCMD_CREATE_OBJECT,
            ObjectType::VertexElements as u32,
            1 + attrs.len() * 4,
        );
        self.push(handle);
        for attr in attrs {
            self.push(attr.offset as u32);
            self.push(0);
            self.push(0);
            self.push(attribute_format(attr.format));
        }
    }

    /// Creates a blend object that writes all channels of color buffer 0 with no blend.
    pub fn create_blend(&mut self, handle: u32) {
        self.header(CCMD_CREATE_OBJECT, ObjectType::Blend as u32, 11);
        self.push(handle);
        self.push(0);
        self.push(0);
        self.push(BLEND_COLORMASK_RGBA);
        for _ in 0..7 {
            self.push(0);
        }
    }

    /// Creates a rasterizer object. It culls no face, fills polygons and
    /// treats counter-clockwise faces as front faces.
    pub fn create_rasterizer(&mut self, handle: u32) {
        self.header(CCMD_CREATE_OBJECT, ObjectType::Rasterizer as u32, 9);
        self.push(handle);
        self.push(RS_DEPTH_CLIP | RS_FRONT_CCW | RS_HALF_PIXEL_CENTER | RS_BOTTOM_EDGE_RULE);
        self.push_f32(1.0);
        self.push(0);
        self.push(0);
        self.push_f32(1.0);
        self.push(0);
        self.push(0);
        self.push(0);
    }

    /// Creates a depth-stencil-alpha object with every test disabled.
    pub fn create_dsa(&mut self, handle: u32) {
        self.header(CCMD_CREATE_OBJECT, ObjectType::Dsa as u32, 5);
        self.push(handle);
        for _ in 0..4 {
            self.push(0);
        }
    }

    /// Creates a surface for level 0 of texture `resource`.
    pub fn create_surface(&mut self, handle: u32, resource: u32) {
        self.header(CCMD_CREATE_OBJECT, ObjectType::Surface as u32, 5);
        self.push(handle);
        self.push(resource);
        self.push(FORMAT_B8G8R8A8_UNORM);
        self.push(0);
        self.push(0);
    }

    pub fn bind_object(&mut self, obj_type: ObjectType, handle: u32) {
        self.header(CCMD_BIND_OBJECT, obj_type as u32, 1);
        self.push(handle);
    }

    pub fn bind_shader(&mut self, handle: u32, kind: ShaderKind) {
        self.header(CCMD_BIND_SHADER, 0, 2);
        self.push(handle);
        self.push(kind as u32);
    }

    /// Makes `surface` the only color buffer; no depth surface.
    pub fn set_framebuffer_state(&mut self, surface: u32) {
        self.header(CCMD_SET_FRAMEBUFFER_STATE, 0, 3);
        self.push(1);
        self.push(0);
        self.push(surface);
    }

    /// Maps clip space to a `width` by `height` target. `y_scale_sign` is
    /// `1.0` or `-1.0` and selects the direction of the y axis.
    pub fn set_viewport(&mut self, width: u32, height: u32, y_scale_sign: f32) {
        let half_width = width as f32 / 2.0;
        let half_height = height as f32 / 2.0;
        self.header(CCMD_SET_VIEWPORT_STATE, 0, 7);
        self.push(0);
        self.push_f32(half_width);
        self.push_f32(y_scale_sign * half_height);
        self.push_f32(0.5);
        self.push_f32(half_width);
        self.push_f32(half_height);
        self.push_f32(0.5);
    }

    /// Binds `resource` as vertex buffer 0 with `stride` bytes between vertices.
    pub fn set_vertex_buffers(&mut self, stride: u32, resource: u32) {
        self.header(CCMD_SET_VERTEX_BUFFERS, 0, 3);
        self.push(stride);
        self.push(0);
        self.push(resource);
    }

    /// Draws `count` vertices as a triangle list from vertex 0.
    pub fn draw_triangles(&mut self, count: u32) {
        self.header(CCMD_DRAW_VBO, 0, 12);
        self.push(0);
        self.push(count);
        self.push(PRIM_TRIANGLES);
        self.push(0);
        self.push(1);
        self.push(0);
        self.push(0);
        self.push(0);
        self.push(0);
        self.push(0);
        self.push(u32::MAX);
        self.push(0);
    }

    /// Clears color buffer 0 to `argb` (`0xAARRGGBB`).
    pub fn clear(&mut self, argb: u32) {
        let channel = |shift: u32| ((argb >> shift) & 0xFF) as f32 / 255.0;
        self.header(CCMD_CLEAR, 0, 8);
        self.push(CLEAR_COLOR0);
        self.push_f32(channel(16));
        self.push_f32(channel(8));
        self.push_f32(channel(0));
        self.push_f32(channel(24));
        // Depth is an `f64` of 0.0 in two dwords; the stencil follows.
        self.push(0);
        self.push(0);
        self.push(0);
    }

    /// Copies `src` of resource `src_res` to `dst` of resource `dst_res`. The copy does not scale.
    pub fn blit(&mut self, dst_res: u32, dst: Rect, src_res: u32, src: Rect) {
        self.header(CCMD_BLIT, 0, 21);
        self.push(BLIT_S0_RGBA_NEAREST);
        self.push(0);
        self.push(0);
        self.blit_target(dst_res, dst);
        self.blit_target(src_res, src);
    }

    /// Writes one `pipe_blit_info` side: level 0 of a 2D texture.
    fn blit_target(&mut self, resource: u32, rect: Rect) {
        self.push(resource);
        self.push(0);
        self.push(FORMAT_B8G8R8A8_UNORM);
        self.push(rect.x);
        self.push(rect.y);
        self.push(0);
        self.push(rect.width);
        self.push(rect.height);
        self.push(1);
    }
}
