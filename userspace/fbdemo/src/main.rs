#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use gfx::api::{CommandRecorder, GfxAllocator, GfxCompiler, GfxQueue, PipelineDesc};
use gfx::backend::software::{SoftAllocator, SoftBackend, SoftCompiler, SoftQueue, SoftShaderDef};
use kernel_abi::gfx::BufferDesc;
use minilib::{
    FbScreenInfo, IoctlRequest, MapFlags, ProtFlags, exit, fsync, heap_init, ioctl, mmap, open,
    write,
};

fn puts(msg: &str) {
    write(1, msg.as_bytes());
}

fn put_u32(n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    let mut value = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    write(1, &buf[i..]);
}

/// Unwraps a gfx pipeline result, reporting `FAIL gfx` and exiting on any error
/// so the whole render path shares one failure line without `unwrap`.
fn or_fail_gfx<T, E>(result: Result<T, E>) -> T {
    match result {
        Ok(v) => v,
        Err(_) => {
            puts("fbdemo: FAIL gfx\n");
            exit(1);
        }
    }
}

// vertex layout: (x, y, r, g, b) — 5 floats × 4 bytes = 20 bytes/vertex
fn color_vert(input: &[f32], output: &mut [f32]) {
    output[0] = input[0]; // NDC x
    output[1] = input[1]; // NDC y
    output[2] = input[2]; // red
    output[3] = input[3]; // green
    output[4] = input[4]; // blue
}

fn color_frag(interp: &[f32]) -> u32 {
    let r = (interp[0].clamp(0.0, 1.0) * 255.0) as u32;
    let g = (interp[1].clamp(0.0, 1.0) * 255.0) as u32;
    let b = (interp[2].clamp(0.0, 1.0) * 255.0) as u32;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    let fd = open("/dev/fb0");
    if fd < 0 {
        puts("fbdemo: FAIL open\n");
        exit(1);
    }

    let mut info = FbScreenInfo::default();
    if ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info) != 0 {
        puts("fbdemo: FAIL ioctl\n");
        exit(1);
    }

    let len = info.pitch as usize * info.height as usize;
    let addr = mmap(
        0,
        len,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        fd as usize,
        0,
    );
    if addr <= 0 {
        puts("fbdemo: FAIL mmap\n");
        exit(1);
    }

    // gfx renders into a heap-backed SoftQueue framebuffer before presenting, so
    // the pool must fit that back buffer (width*height*4 ≈ len) plus rasterizer
    // churn; the extra megabyte is slack for the transient allocations.
    if !heap_init(len + (1 << 20)) {
        puts("fbdemo: FAIL heap\n");
        exit(1);
    }

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let vert = or_fail_gfx(backend.compile_shader(&SoftShaderDef::Vertex {
        func: color_vert,
        output_count: 5,
    }));
    let frag = or_fail_gfx(backend.compile_shader(&SoftShaderDef::Fragment(color_frag)));
    let pso = or_fail_gfx(backend.compile_pipeline(&PipelineDesc {
        vertex_shader: &vert,
        pixel_shader: &frag,
        blend: false,
        depth: false,
        vertex_stride: 20,
    }));

    // three vertices: red apex top, green bottom-left, blue bottom-right
    #[rustfmt::skip]
    let verts: [f32; 15] = [
         0.00,  0.75, 1.0, 0.0, 0.0,
        -0.75, -0.75, 0.0, 1.0, 0.0,
         0.75, -0.75, 0.0, 0.0, 1.0,
    ];
    let bytes: Vec<u8> = verts.iter().flat_map(|f| f.to_ne_bytes()).collect();

    let mut vbuf = or_fail_gfx(backend.alloc_buffer(&BufferDesc {
        size: 60,
        is_dynamic: false,
    }));
    vbuf.data.copy_from_slice(&bytes);

    let mut q = SoftQueue::new(info.width, info.height);
    or_fail_gfx(q.submit(|rec| {
        rec.bind_pipeline(&pso);
        rec.bind_vertex_buffer(&vbuf);
        rec.draw(3);
    }));

    let stride = info.pitch as usize / 4;
    let width = info.width as usize;
    let height = info.height as usize;
    // SAFETY: the mmap covers `len` bytes and is page-aligned, hence u32-aligned;
    // `stride * (height - 1) + width` u32 words stay within that mapping.
    let fb_u32 = unsafe {
        core::slice::from_raw_parts_mut(addr as usize as *mut u32, stride * (height - 1) + width)
    };
    q.present_into(fb_u32, stride);

    if fsync(fd) != 0 {
        puts("fbdemo: FAIL fsync\n");
        exit(1);
    }

    puts("fbdemo: frame drawn ");
    put_u32(info.width);
    puts("x");
    put_u32(info.height);
    puts("\n");
    exit(0);
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &::core::panic::PanicInfo) -> ! {
    loop {}
}
