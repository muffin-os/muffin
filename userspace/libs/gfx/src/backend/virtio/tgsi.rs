//! The gouraud pipeline that the demo and the compositor share: a clip-space
//! position in `IN[0]` and an RGB color in `IN[1]`.

use crate::api::{AttributeFormat, VertexAttribute};

/// Vertex shader: `OUT[0] = (x, y, 0, 1)`, `OUT[1] = color`.
pub const GOURAUD_VERTEX: &str = "VERT\n\
DCL IN[0]\n\
DCL IN[1]\n\
DCL OUT[0], POSITION\n\
DCL OUT[1], COLOR\n\
IMM[0] FLT32 {0.0, 1.0, 0.0, 0.0}\n\
MOV OUT[0].xy, IN[0].xyxx\n\
MOV OUT[0].zw, IMM[0].xxxy\n\
MOV OUT[1], IN[1]\n\
END\n";

/// Fragment shader: writes the interpolated color.
pub const GOURAUD_FRAGMENT: &str = "FRAG\n\
DCL IN[0], COLOR, PERSPECTIVE\n\
DCL OUT[0], COLOR\n\
MOV OUT[0], IN[0]\n\
END\n";

/// Vertex layout `(x, y, r, g, b)` as `f32`.
pub const GOURAUD_ATTRIBUTES: [VertexAttribute; 2] = [
    VertexAttribute {
        offset: 0,
        format: AttributeFormat::F32x2,
    },
    VertexAttribute {
        offset: 8,
        format: AttributeFormat::F32x3,
    },
];

/// Bytes between the start of consecutive gouraud vertices.
pub const GOURAUD_STRIDE: usize = 5 * size_of::<f32>();
