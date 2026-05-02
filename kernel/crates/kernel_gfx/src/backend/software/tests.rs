use alloc::vec::Vec;

use kernel_abi::gfx::BufferDesc;

use crate::api::{
    Adapter, CommandRecorder, GfxAllocator, GfxCompiler, GfxFence, GfxQueue, PipelineDesc,
};
use crate::backend::software::{
    SoftAdapter, SoftAllocator, SoftBackend, SoftCompiler, SoftFence, SoftQueue, SoftShaderDef,
};

// ── shared shader functions ──────────────────────────────────────────────────

fn passthrough_vert(input: &[f32], output: &mut [f32]) {
    output[0] = input[0];
    output[1] = input[1];
}

fn white_frag(_: &[f32]) -> u32 {
    0xFFFF_FFFF
}

// Vertex layout for Gouraud shading: (x, y, r, g, b) — 5 floats = 20 bytes.
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

// ── helpers ──────────────────────────────────────────────────────────────────

fn encode_verts(floats: &[f32]) -> Vec<u8> {
    floats.iter().flat_map(|f| f.to_ne_bytes()).collect()
}

/// Build a backend and a simple (passthrough-vert, white-frag) pipeline.
/// `vertex_stride` is the byte size of one vertex in the buffer.
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
            blend: false,
            depth: false,
            vertex_stride,
        })
        .unwrap()
}

// ── tests ────────────────────────────────────────────────────────────────────

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
fn blank_framebuffer_is_all_zero() {
    let q = SoftQueue::new(4, 4);
    assert!(q.framebuffer().iter().all(|&p| p == 0));
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

    let mut q = SoftQueue::new(4, 4);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(0);
    })
    .unwrap();

    assert!(q.framebuffer().iter().all(|&p| p == 0));
}

#[test]
fn triangle_writes_correct_pixels() {
    // CCW triangle in NDC; screen corners in a 4×4 framebuffer:
    //   v0 (-0.5,  0.5) → screen (1, 1)
    //   v1 ( 0.5,  0.5) → screen (3, 1)
    //   v2 (-0.5, -0.5) → screen (1, 3)
    let bytes = encode_verts(&[-0.5, 0.5, 0.5, 0.5, -0.5, -0.5]);

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    // Each vertex is 2 floats = 8 bytes.
    let pso = simple_pipeline(&mut backend, 8);
    let mut buf = backend
        .alloc_buffer(&BufferDesc {
            size: bytes.len(),
            is_dynamic: false,
        })
        .unwrap();
    buf.data.copy_from_slice(&bytes);

    let mut q = SoftQueue::new(4, 4);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(3);
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
        blend: false,
        depth: false,
        vertex_stride: 8,
    });
    assert!(result.is_err());
}

// ── interpolation test ───────────────────────────────────────────────────────

// Vertex layout for the interpolation test: (x, y, intensity) — 3 floats = 12 bytes.
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
            blend: false,
            depth: false,
            vertex_stride: 12, // 3 floats × 4 bytes
        })
        .unwrap();

    let mut buf = backend
        .alloc_buffer(&BufferDesc {
            size: bytes.len(),
            is_dynamic: false,
        })
        .unwrap();
    buf.data.copy_from_slice(&bytes);

    let mut q = SoftQueue::new(4, 4);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(3);
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

// ── Gouraud shading ──────────────────────────────────────────────────────────

#[test]
fn gouraud_triangle_blends_per_vertex_colors() {
    // Each vertex carries its own RGB colour; the rasterizer interpolates all
    // three channels across the triangle (Gouraud shading).
    //
    // Vertex layout: (x, y, r, g, b) — 5 floats × 4 bytes = 20 bytes/vertex.
    // output_count = 5: two for NDC position + three colour interpolants.
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
            blend: false,
            depth: false,
            vertex_stride: 20,
        })
        .unwrap();

    let mut buf = backend
        .alloc_buffer(&BufferDesc {
            size: bytes.len(),
            is_dynamic: false,
        })
        .unwrap();
    buf.data.copy_from_slice(&bytes);

    let mut q = SoftQueue::new(8, 8);
    q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&buf);
        rec.draw(3);
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
