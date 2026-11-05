use crate::api::{AttributeFormat, Rect, VertexAttribute};
use crate::backend::virtio::encode::{ObjectType, ShaderKind, Stream};
use crate::backend::virtio::tgsi::{GOURAUD_ATTRIBUTES, GOURAUD_FRAGMENT, GOURAUD_VERTEX};
use crate::backend::virtio::{VirtioPipeline, VirtioShader, clip_blit};
use crate::error::GfxError;

/// `VIRGL_CMD0(cmd, obj, len)`.
fn cmd0(cmd: u32, obj: u32, len: u32) -> u32 {
    cmd | (obj << 8) | (len << 16)
}

#[test]
fn bind_object_packs_header() {
    let mut stream = Stream::new();
    stream.bind_object(ObjectType::Blend, 7);
    assert_eq!(stream.words(), &[cmd0(2, 1, 1), 7]);
    assert_eq!(stream.as_bytes().len(), 8);
}

#[test]
fn create_shader_pads_text_to_dwords() {
    let mut stream = Stream::new();
    stream
        .create_shader(3, ShaderKind::Fragment, "FRAG\n0")
        .unwrap();
    // Header, five header dwords, and two text dwords for 6 bytes plus NUL.
    assert_eq!(stream.words().len(), 8);
    assert_eq!(stream.words()[0], cmd0(1, 4, 7));
    assert_eq!(&stream.words()[1..6], &[3, 1, 7, 6, 0]);
    assert_eq!(stream.words()[6], u32::from_ne_bytes(*b"FRAG"));
    assert_eq!(stream.words()[7], u32::from_ne_bytes(*b"\n0\0\0"));

    let mut stream = Stream::new();
    stream
        .create_shader(4, ShaderKind::Vertex, "VERT\n01")
        .unwrap();
    // 7 bytes plus NUL fill two dwords exactly.
    assert_eq!(stream.words().len(), 8);
    assert_eq!(stream.words()[0], cmd0(1, 4, 7));
    assert_eq!(&stream.words()[1..6], &[4, 0, 8, 7, 0]);
    assert_eq!(stream.words()[7], u32::from_ne_bytes(*b"\n01\0"));

    let mut stream = Stream::new();
    stream
        .create_shader(5, ShaderKind::Vertex, "VERT\n012")
        .unwrap();
    // 8 bytes need a third dword for the NUL.
    assert_eq!(stream.words().len(), 9);
    assert_eq!(stream.words()[8], 0);
}

#[test]
fn create_shader_rejects_text_over_one_command() {
    let text = "A".repeat(300_000);
    let mut stream = Stream::new();
    assert_eq!(
        stream.create_shader(1, ShaderKind::Vertex, &text),
        Err(GfxError::InvalidShaderSource)
    );
    assert!(stream.is_empty());
}

#[test]
fn blit_places_resources_and_rects() {
    let mut stream = Stream::new();
    let dst = Rect {
        x: 10,
        y: 20,
        width: 30,
        height: 40,
    };
    let src = Rect {
        x: 1,
        y: 2,
        width: 30,
        height: 40,
    };
    stream.blit(9, dst, 8, src);
    let words = stream.words();
    assert_eq!(words.len(), 22);
    assert_eq!(words[0], cmd0(16, 0, 21));
    assert_eq!(words[1], 0xF);
    assert_eq!(words[4], 9);
    assert_eq!(&words[5..13], &[0, 1, 10, 20, 0, 30, 40, 1]);
    assert_eq!(words[13], 8);
    assert_eq!(&words[14..22], &[0, 1, 1, 2, 0, 30, 40, 1]);
}

#[test]
fn clear_encodes_channels_as_floats() {
    let mut stream = Stream::new();
    stream.clear(0xFF20_60C0);
    let words = stream.words();
    assert_eq!(words.len(), 9);
    assert_eq!(words[0], cmd0(7, 0, 8));
    assert_eq!(words[1], 1 << 2);
    assert_eq!(f32::from_bits(words[2]), 0x20 as f32 / 255.0);
    assert_eq!(f32::from_bits(words[3]), 0x60 as f32 / 255.0);
    assert_eq!(f32::from_bits(words[4]), 0xC0 as f32 / 255.0);
    assert_eq!(f32::from_bits(words[5]), 1.0);
    assert_eq!(&words[6..9], &[0, 0, 0]);
}

#[test]
fn draw_triangles_sets_count_and_mode() {
    let mut stream = Stream::new();
    stream.draw_triangles(96);
    let words = stream.words();
    assert_eq!(words.len(), 13);
    assert_eq!(words[0], cmd0(8, 0, 12));
    assert_eq!(words[2], 96);
    assert_eq!(words[3], 4);
    assert_eq!(words[5], 1);
    assert_eq!(words[11], u32::MAX);
}

#[test]
fn create_vertex_elements_uses_float_formats() {
    let mut stream = Stream::new();
    stream.create_vertex_elements(2, &GOURAUD_ATTRIBUTES);
    let words = stream.words();
    assert_eq!(words[0], cmd0(1, 5, 9));
    assert_eq!(&words[1..10], &[2, 0, 0, 0, 29, 8, 0, 0, 30]);
}

#[test]
fn state_objects_have_protocol_sizes() {
    let mut stream = Stream::new();
    stream.create_blend(1);
    stream.create_rasterizer(2);
    stream.create_dsa(3);
    stream.create_surface(4, 5);
    stream.bind_shader(6, ShaderKind::Fragment);
    stream.set_framebuffer_state(4);
    stream.set_viewport(800, 600, 1.0);
    stream.set_vertex_buffers(20, 7);
    let words = stream.words();
    assert_eq!(words[0], cmd0(1, 1, 11));
    assert_eq!(words[4], 0xF << 27);
    assert_eq!(words[12], cmd0(1, 2, 9));
    assert_eq!(words[14], (1 << 1) | (1 << 15) | (1 << 29) | (1 << 30));
    assert_eq!(f32::from_bits(words[15]), 1.0);
    assert_eq!(words[22], cmd0(1, 3, 5));
    assert_eq!(words[28], cmd0(1, 8, 5));
    assert_eq!(&words[29..34], &[4, 5, 1, 0, 0]);
    assert_eq!(words[34], cmd0(31, 0, 2));
    assert_eq!(&words[35..37], &[6, 1]);
    assert_eq!(words[37], cmd0(5, 0, 3));
    assert_eq!(&words[38..41], &[1, 0, 4]);
    assert_eq!(words[41], cmd0(4, 0, 7));
    assert_eq!(words[42], 0);
    assert_eq!(f32::from_bits(words[43]), 400.0);
    assert_eq!(f32::from_bits(words[44]), 300.0);
    assert_eq!(f32::from_bits(words[45]), 0.5);
    assert_eq!(f32::from_bits(words[46]), 400.0);
    assert_eq!(f32::from_bits(words[47]), 300.0);
    assert_eq!(words[49], cmd0(6, 0, 3));
    assert_eq!(&words[50..53], &[20, 0, 7]);
}

#[test]
fn set_viewport_applies_y_sign() {
    let mut stream = Stream::new();
    stream.set_viewport(800, 600, -1.0);
    assert_eq!(f32::from_bits(stream.words()[3]), -300.0);
    assert_eq!(f32::from_bits(stream.words()[6]), 300.0);
}

#[test]
fn shader_parse_reads_stage() {
    assert_eq!(
        VirtioShader::parse(GOURAUD_VERTEX).unwrap().kind(),
        ShaderKind::Vertex
    );
    assert_eq!(
        VirtioShader::parse(GOURAUD_FRAGMENT).unwrap().kind(),
        ShaderKind::Fragment
    );
    assert_eq!(
        VirtioShader::parse("COMP\nEND\n"),
        Err(GfxError::InvalidShaderSource)
    );
    assert_eq!(
        VirtioShader::parse("VERT"),
        Err(GfxError::InvalidShaderSource)
    );
}

#[test]
fn pipeline_encode_validates_and_emits_objects() {
    let vertex = VirtioShader::parse(GOURAUD_VERTEX).unwrap();
    let fragment = VirtioShader::parse(GOURAUD_FRAGMENT).unwrap();
    let handles = [1, 2, 3, 4, 5, 6];

    let mut stream = Stream::new();
    let pipeline = VirtioPipeline::encode(
        &mut stream,
        handles,
        &vertex,
        &fragment,
        20,
        &GOURAUD_ATTRIBUTES,
    )
    .unwrap();
    assert_eq!(pipeline.stride(), 20);
    let words = stream.words();
    assert_eq!(words[0] & 0xFFFF, cmd0(1, 4, 0));
    assert_eq!(words[1], 1);
    // The last object is the DSA with handle 6.
    assert_eq!(&words[words.len() - 5..], &[6, 0, 0, 0, 0]);

    let mut bound = Stream::new();
    pipeline.bind(&mut bound);
    assert_eq!(
        bound.words(),
        &[
            cmd0(2, 1, 1),
            4,
            cmd0(2, 2, 1),
            5,
            cmd0(2, 3, 1),
            6,
            cmd0(2, 5, 1),
            3,
            cmd0(31, 0, 2),
            1,
            0,
            cmd0(31, 0, 2),
            2,
            1,
        ]
    );

    let mut stream = Stream::new();
    assert_eq!(
        VirtioPipeline::encode(
            &mut stream,
            handles,
            &fragment,
            &vertex,
            20,
            &GOURAUD_ATTRIBUTES
        ),
        Err(GfxError::WrongShaderKind)
    );
    assert_eq!(
        VirtioPipeline::encode(
            &mut stream,
            handles,
            &vertex,
            &fragment,
            18,
            &GOURAUD_ATTRIBUTES
        ),
        Err(GfxError::InvalidVertexStride)
    );
    let too_wide = [VertexAttribute {
        offset: 8,
        format: AttributeFormat::F32x4,
    }];
    assert_eq!(
        VirtioPipeline::encode(&mut stream, handles, &vertex, &fragment, 20, &too_wide),
        Err(GfxError::InvalidVertexAttribute)
    );
    assert!(stream.is_empty());
}

#[test]
fn clip_blit_clips_to_both_images() {
    let rect = Rect {
        x: 2,
        y: 3,
        width: 10,
        height: 10,
    };
    let (src, dst) = clip_blit(8, 8, rect, 1, 2, 5, 100).unwrap();
    assert_eq!(src.width, 4);
    assert_eq!(src.height, 5);
    assert_eq!((src.x, src.y), (2, 3));
    assert_eq!((dst.x, dst.y, dst.width, dst.height), (1, 2, 4, 5));
    assert_eq!(clip_blit(8, 8, rect, 5, 0, 5, 5), None);
    assert_eq!(clip_blit(2, 8, rect, 0, 0, 5, 5), None);
}
