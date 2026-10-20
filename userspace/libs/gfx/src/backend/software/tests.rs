use alloc::rc::Rc;
use alloc::vec::Vec;

use kernel_abi::gfx::BufferDesc;

use crate::api::{
    Adapter, CommandRecorder, GfxAllocator, GfxCompiler, GfxFence, GfxQueue, PipelineDesc,
};
use crate::backend::software::{
    ATTRIBUTE_BYTES, SoftAdapter, SoftAllocator, SoftBackend, SoftCompiler, SoftFence, SoftQueue,
    SoftRecorder, SoftShaderDef,
};
use crate::error::GfxError;

fn passthrough_vert(input: &[f32], output: &mut [f32]) {
    output[0] = input[0];
    output[1] = input[1];
}

fn white_frag(_: &[f32]) -> u32 {
    0xFFFF_FFFF
}

// Vertex layout (x, y, r, g, b).
fn color_vert(input: &[f32], output: &mut [f32]) {
    output[0] = input[0]; // NDC x
    output[1] = input[1]; // NDC y
    output[2] = input[2]; // red channel [0, 1]
    output[3] = input[3]; // green channel [0, 1]
    output[4] = input[4]; // blue channel [0, 1]
}

// Reconstruct a packed 0xFFRRGGBB colour from three interpolated channels.
fn color_frag(interp: &[f32]) -> u32 {
    let r = (interp[0].clamp(0.0, 1.0) * 255.0) as u32;
    let g = (interp[1].clamp(0.0, 1.0) * 255.0) as u32;
    let b = (interp[2].clamp(0.0, 1.0) * 255.0) as u32;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

fn encode_verts(floats: &[f32]) -> Vec<u8> {
    floats.iter().flat_map(|f| f.to_ne_bytes()).collect()
}

/// Compiles a passthrough-vert, white-frag pipeline on `backend`.
fn simple_pipeline(
    backend: &mut SoftBackend,
    vertex_stride: usize,
) -> crate::backend::software::SoftPipeline {
    let vert = backend
        .compile_shader(&SoftShaderDef::Vertex {
            func: passthrough_vert,
            output_count: 2,
        })
        .unwrap();
    let frag = backend
        .compile_shader(&SoftShaderDef::Fragment(white_frag))
        .unwrap();
    backend
        .compile_pipeline(&PipelineDesc {
            vertex_shader: &vert,
            pixel_shader: &frag,
            vertex_stride,
        })
        .unwrap()
}

#[test]
fn adapter_is_not_hw_accelerated() {
    let a = SoftAdapter;
    assert!(!a.is_hardware_accelerated());
    assert!(a.max_texture_resolution() > 0);
}

#[test]
fn fence_is_always_ready() {
    let f = SoftFence;
    assert!(f.is_ready());
    f.wait();
}

#[test]
fn new_does_not_write_target() {
    // A zeroed target could not distinguish a queue that leaves the target alone
    // from one that clears it.
    let mut target = [0xDEAD_BEEFu32; 16];
    let q = SoftQueue::new(&mut target, 4, 4, 4);
    assert!(q.framebuffer().iter().all(|&p| p == 0xDEAD_BEEF));
}

#[test]
#[should_panic(expected = "height 0")]
fn new_rejects_zero_height() {
    let mut target = [0u32; 16];
    let _ = SoftQueue::new(&mut target, 4, 0, 4);
}

#[test]
#[should_panic(expected = "too small for 4x4 at stride 16px")]
fn new_rejects_byte_pitch_passed_as_pixel_stride() {
    // A 4x4 u32 target has a 16 byte pitch; handing that pitch to `stride_px`
    // asks for four times the pixels the target holds.
    let mut target = [0u32; 16];
    let _ = SoftQueue::new(&mut target, 4, 4, 16);
}

#[test]
fn no_draw_leaves_framebuffer_blank() {
    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let pso = simple_pipeline(&mut backend, 8);
    let buf = backend
        .alloc_buffer(&BufferDesc {
            size: 0,
            is_dynamic: false,
        })
        .unwrap();

    let mut target = [0u32; 16];
    let mut q = SoftQueue::new(&mut target, 4, 4, 4);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(0)
    })
    .unwrap();

    assert!(q.framebuffer().iter().all(|&p| p == 0));
}

#[test]
fn triangle_writes_correct_pixels() {
    // CW triangle in NDC. Screen corners in a 4×4 framebuffer:
    //   v0 (-0.5,  0.5) → screen (1, 1)
    //   v1 ( 0.5,  0.5) → screen (3, 1)
    //   v2 (-0.5, -0.5) → screen (1, 3)
    let bytes = encode_verts(&[-0.5, 0.5, 0.5, 0.5, -0.5, -0.5]);

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let pso = simple_pipeline(&mut backend, 2 * ATTRIBUTE_BYTES);
    let mut buf = backend
        .alloc_buffer(&BufferDesc {
            size: bytes.len(),
            is_dynamic: false,
        })
        .unwrap();
    buf.data.copy_from_slice(&bytes);

    let mut target = [0u32; 16];
    let mut q = SoftQueue::new(&mut target, 4, 4, 4);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(3)
    })
    .unwrap();

    let fb = q.framebuffer();
    assert_eq!(fb[5], 0xFFFF_FFFF, "pixel (1,1) should be inside");
    assert_eq!(fb[3 * 4 + 3], 0x0000_0000, "pixel (3,3) should be outside");
    assert_eq!(fb[0], 0x0000_0000, "pixel (0,0) should be outside");
}

#[test]
fn wrong_shader_kind_is_rejected() {
    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let vert = backend
        .compile_shader(&SoftShaderDef::Vertex {
            func: passthrough_vert,
            output_count: 2,
        })
        .unwrap();
    // Pass a vertex shader in both slots — should fail.
    let result = backend.compile_pipeline(&PipelineDesc {
        vertex_shader: &vert,
        pixel_shader: &vert,
        vertex_stride: 2 * ATTRIBUTE_BYTES,
    });
    assert!(result.is_err());
}

#[test]
fn draw_without_pipeline_is_rejected() {
    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let buf = backend
        .alloc_buffer(&BufferDesc {
            size: 6 * ATTRIBUTE_BYTES,
            is_dynamic: false,
        })
        .unwrap();

    let mut target = [0xDEAD_BEEFu32; 16];
    let mut q = SoftQueue::new(&mut target, 4, 4, 4);
    let result = q.submit(|rec| {
        rec.bind_vertex_buffer(&buf);
        rec.draw(3)
    });

    assert_eq!(result, Err(GfxError::PipelineNotBound));
    assert!(q.framebuffer().iter().all(|&p| p == 0xDEAD_BEEF));
}

#[test]
fn draw_without_vertex_buffer_is_rejected() {
    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let pso = simple_pipeline(&mut backend, 2 * ATTRIBUTE_BYTES);

    let mut target = [0xDEAD_BEEFu32; 16];
    let mut q = SoftQueue::new(&mut target, 4, 4, 4);
    let result = q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.draw(3)
    });

    assert_eq!(result, Err(GfxError::VertexBufferNotBound));
    assert!(q.framebuffer().iter().all(|&p| p == 0xDEAD_BEEF));
}

#[test]
fn invalid_pipeline_parameters_are_rejected() {
    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let vert = SoftShaderDef::Vertex {
        func: passthrough_vert,
        output_count: 2,
    };
    let frag = SoftShaderDef::Fragment(white_frag);

    for stride in [0, ATTRIBUTE_BYTES + 1] {
        let result = backend.compile_pipeline(&PipelineDesc {
            vertex_shader: &vert,
            pixel_shader: &frag,
            vertex_stride: stride,
        });
        assert_eq!(
            result.err(),
            Some(GfxError::InvalidVertexStride),
            "stride {stride} is not a whole number of attributes"
        );
    }

    let position_only = SoftShaderDef::Vertex {
        func: passthrough_vert,
        output_count: 1,
    };
    let result = backend.compile_pipeline(&PipelineDesc {
        vertex_shader: &position_only,
        pixel_shader: &frag,
        vertex_stride: 2 * ATTRIBUTE_BYTES,
    });
    assert_eq!(result.err(), Some(GfxError::InvalidOutputCount));
}

#[test]
fn draws_share_the_bound_vertex_bytes() {
    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let pso = simple_pipeline(&mut backend, 2 * ATTRIBUTE_BYTES);
    let buf = backend
        .alloc_buffer(&BufferDesc {
            size: 6 * ATTRIBUTE_BYTES,
            is_dynamic: false,
        })
        .unwrap();

    let mut rec = SoftRecorder::new();
    rec.bind_pipeline(&pso);
    rec.bind_vertex_buffer(&buf);
    rec.draw(3).unwrap();
    rec.draw(3).unwrap();

    assert!(
        Rc::ptr_eq(&rec.cmds[0].raw_data, &rec.cmds[1].raw_data),
        "a second draw against the same binding must not copy the vertex bytes"
    );
}

// Vertex layout (x, y, intensity).
fn intensity_vert(input: &[f32], output: &mut [f32]) {
    output[0] = input[0]; // NDC x
    output[1] = input[1]; // NDC y
    output[2] = input[2]; // intensity interpolant
}

// Returns blue if interpolant > 0.5, green otherwise — no floating-point
// arithmetic in the assertion, so the test is exact.
fn threshold_frag(interp: &[f32]) -> u32 {
    if interp[0] > 0.5 {
        0xFF_00_00_FF
    } else {
        0xFF_00_FF_00
    }
}

#[test]
fn interpolants_vary_across_triangle() {
    // Triangle pointing up; apex at screen center, base at bottom edge.
    // v0 (0.0,  0.0) → screen (2, 2) — intensity 1.0 (high)
    // v1 (-1.0,-1.0) → screen (0, 4) — intensity 0.0 (low)
    // v2 ( 1.0,-1.0) → screen (4, 4) — intensity 0.0 (low)
    //
    // Barycentric weights at pixel (2,2) center (2.5, 2.5):
    //   total_area = (0-2)*(4-2) - (4-2)*(4-2) = -8
    //   w0 ≈ 0.75  → intensity ≈ 0.75  → > 0.5  → blue
    //
    // Barycentric weights at pixel (2,3) center (2.5, 3.5):
    //   w0 ≈ 0.25  → intensity ≈ 0.25  → < 0.5  → green
    let bytes = encode_verts(&[0.0, 0.0, 1.0, -1.0, -1.0, 0.0, 1.0, -1.0, 0.0]);

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let vert = backend
        .compile_shader(&SoftShaderDef::Vertex {
            func: intensity_vert,
            output_count: 3,
        })
        .unwrap();
    let frag = backend
        .compile_shader(&SoftShaderDef::Fragment(threshold_frag))
        .unwrap();
    let pso = backend
        .compile_pipeline(&PipelineDesc {
            vertex_shader: &vert,
            pixel_shader: &frag,
            vertex_stride: 3 * ATTRIBUTE_BYTES,
        })
        .unwrap();

    let mut buf = backend
        .alloc_buffer(&BufferDesc {
            size: bytes.len(),
            is_dynamic: false,
        })
        .unwrap();
    buf.data.copy_from_slice(&bytes);

    let mut target = [0u32; 16];
    let mut q = SoftQueue::new(&mut target, 4, 4, 4);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(3)
    })
    .unwrap();

    let fb = q.framebuffer();
    assert_eq!(
        fb[2 * 4 + 2],
        0xFF_00_00_FF,
        "pixel (2,2) near apex → intensity > 0.5 → blue"
    );
    assert_eq!(
        fb[3 * 4 + 2],
        0xFF_00_FF_00,
        "pixel (2,3) near base → intensity < 0.5 → green"
    );
}

#[test]
fn gouraud_triangle_blends_per_vertex_colors() {
    // Each vertex carries its own RGB colour; the rasterizer interpolates all
    // three channels across the triangle (Gouraud shading).
    //
    // NDC → screen  (x' = (x+1)·4,  y' = (1−y)·4)  for an 8×8 framebuffer:
    //   v0 ( 0.00,  0.75) RED   → screen (4, 1)
    //   v1 (−0.75, −0.75) GREEN → screen (1, 7)
    //   v2 ( 0.75, −0.75) BLUE  → screen (7, 7)
    //
    // Barycentric weights at the tested pixel centres
    // (total_area = −36 for this triangle):
    //   pixel (4,2) cx=4.5 cy=2.5 → w=(0.75, 0.042, 0.208) → red dominant
    //   pixel (2,6) cx=2.5 cy=6.5 → w=(0.083, 0.708, 0.208) → green dominant
    //   pixel (6,6) cx=6.5 cy=6.5 → w=(0.083, 0.042, 0.875) → blue dominant
    #[rustfmt::skip]
    let bytes = encode_verts(&[
         0.00,  0.75, 1.0, 0.0, 0.0,  // v0 — pure red
        -0.75, -0.75, 0.0, 1.0, 0.0,  // v1 — pure green
         0.75, -0.75, 0.0, 0.0, 1.0,  // v2 — pure blue
    ]);

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let vert = backend
        .compile_shader(&SoftShaderDef::Vertex {
            func: color_vert,
            output_count: 5,
        })
        .unwrap();
    let frag = backend
        .compile_shader(&SoftShaderDef::Fragment(color_frag))
        .unwrap();
    let pso = backend
        .compile_pipeline(&PipelineDesc {
            vertex_shader: &vert,
            pixel_shader: &frag,
            vertex_stride: 5 * ATTRIBUTE_BYTES,
        })
        .unwrap();

    let mut buf = backend
        .alloc_buffer(&BufferDesc {
            size: bytes.len(),
            is_dynamic: false,
        })
        .unwrap();
    buf.data.copy_from_slice(&bytes);

    let mut target = [0u32; 64];
    let mut q = SoftQueue::new(&mut target, 8, 8, 8);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(3)
    })
    .unwrap();

    let fb = q.framebuffer();
    let ch = |p: u32| ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);

    let (r, g, b) = ch(fb[2 * 8 + 4]);
    assert!(
        r > g && r > b,
        "pixel (4,2) near red apex: r={r} g={g} b={b}"
    );

    let (r, g, b) = ch(fb[6 * 8 + 2]);
    assert!(
        g > r && g > b,
        "pixel (2,6) near green vertex: r={r} g={g} b={b}"
    );

    let (r, g, b) = ch(fb[6 * 8 + 6]);
    assert!(
        b > r && b > g,
        "pixel (6,6) near blue vertex: r={r} g={g} b={b}"
    );
}

#[test]
fn draw_respects_stride_padding() {
    // A 3x2 target at stride 4 has one padding column per row, at indices 3 and 7.
    // Indexing a pixel store by width corrupts those columns.
    let bytes = encode_verts(&[-2.0, 2.0, 2.0, 2.0, 0.0, -2.0]);

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let pso = simple_pipeline(&mut backend, 8);
    let mut buf = backend
        .alloc_buffer(&BufferDesc {
            size: bytes.len(),
            is_dynamic: false,
        })
        .unwrap();
    buf.data.copy_from_slice(&bytes);

    let mut target = [0xDEAD_BEEFu32; 8];
    let mut q = SoftQueue::new(&mut target, 3, 2, 4);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(3)
    })
    .unwrap();

    for (i, &px) in target.iter().enumerate() {
        let expected = if i == 3 || i == 7 {
            0xDEAD_BEEF
        } else {
            0xFFFF_FFFF
        };
        assert_eq!(px, expected, "pixel at index {i} mismatch");
    }
}

fn blue_frag(_: &[f32]) -> u32 {
    0xFF00_00FF
}

#[test]
fn each_draw_uses_the_pipeline_bound_at_draw_time() {
    // Two triangles on a 2×2 target, one covering row 0 and the other row 1,
    // so every pixel is written by exactly one draw.
    let top = encode_verts(&[-2.0, 2.0, 2.0, 2.0, 0.0, 0.0]);
    let bottom = encode_verts(&[-2.0, 0.0, 2.0, 0.0, 0.0, -2.0]);

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let white = simple_pipeline(&mut backend, 2 * ATTRIBUTE_BYTES);
    let vert = backend
        .compile_shader(&SoftShaderDef::Vertex {
            func: passthrough_vert,
            output_count: 2,
        })
        .unwrap();
    let frag = backend
        .compile_shader(&SoftShaderDef::Fragment(blue_frag))
        .unwrap();
    let blue = backend
        .compile_pipeline(&PipelineDesc {
            vertex_shader: &vert,
            pixel_shader: &frag,
            vertex_stride: 2 * ATTRIBUTE_BYTES,
        })
        .unwrap();
    let mut top_buf = backend
        .alloc_buffer(&BufferDesc {
            size: top.len(),
            is_dynamic: false,
        })
        .unwrap();
    top_buf.data.copy_from_slice(&top);
    let mut bottom_buf = backend
        .alloc_buffer(&BufferDesc {
            size: bottom.len(),
            is_dynamic: false,
        })
        .unwrap();
    bottom_buf.data.copy_from_slice(&bottom);

    let mut target = [0u32; 4];
    let mut q = SoftQueue::new(&mut target, 2, 2, 2);
    q.submit(|rec| {
        rec.bind_pipeline(&white);
        rec.bind_vertex_buffer(&top_buf);
        rec.draw(3)?;
        rec.bind_pipeline(&blue);
        rec.bind_vertex_buffer(&bottom_buf);
        rec.draw(3)
    })
    .unwrap();

    assert_eq!(target, [0xFFFF_FFFF, 0xFFFF_FFFF, 0xFF00_00FF, 0xFF00_00FF]);
}
