#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use gfx::api::{CommandRecorder, GfxAllocator, GfxCompiler, GfxQueue, PipelineDesc};
use gfx::backend::software::{SoftAllocator, SoftBackend, SoftCompiler, SoftQueue, SoftShaderDef};
use kernel_abi::gfx::BufferDesc;
use minilib::{
    CLOCK_MONOTONIC, FbScreenInfo, IoctlRequest, MapFlags, ProtFlags, Timespec, clock_gettime,
    exit, fsync, ioctl, mmap, open, println,
};

// monotonic timestamp in microseconds; on failure the zeroed timespec yields 0
fn now_us() -> u64 {
    let mut tp = Timespec::default();
    let _ = clock_gettime(CLOCK_MONOTONIC, &mut tp);
    (tp.tv_sec as u64) * 1_000_000 + (tp.tv_nsec as u64) / 1000
}

fn report(label: &str, start_us: u64, end_us: u64) {
    println!("fbdemo: {label} {}us", end_us.saturating_sub(start_us));
}

/// Unwraps a gfx pipeline result, reporting `FAIL gfx` and exiting on any error
/// so the whole render path shares one failure line without `unwrap`.
fn or_fail_gfx<T, E>(result: Result<T, E>) -> T {
    match result {
        Ok(v) => v,
        Err(_) => {
            println!("fbdemo: FAIL gfx");
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

/// Scales a unit-interval channel to 0..=255, saturating outside it.
///
/// Runs three times per covered pixel, so it must not become an out-of-line
/// call. In an unoptimised build only `inline(always)` guarantees that.
#[inline(always)]
fn channel_to_u8(value: f32) -> u32 {
    if value <= 0.0 {
        0
    } else if value >= 1.0 {
        255
    } else {
        (value * 255.0) as u32
    }
}

fn color_frag(interp: &[f32]) -> u32 {
    let r = channel_to_u8(interp[0]);
    let g = channel_to_u8(interp[1]);
    let b = channel_to_u8(interp[2]);
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

minilib::entry!(main);

fn main() -> i32 {
    let t = now_us();
    let fd = open("/dev/fb0");
    report("open", t, now_us());
    let Ok(fd) = fd else {
        println!("fbdemo: FAIL open");
        return 1;
    };

    let mut info = FbScreenInfo::default();
    let t = now_us();
    let ioctl_rc = ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info);
    report("ioctl", t, now_us());
    if ioctl_rc.is_err() {
        println!("fbdemo: FAIL ioctl");
        return 1;
    }

    let len = info.pitch as usize * info.height as usize;
    let t = now_us();
    let mapped = mmap(
        0,
        len,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        fd as usize,
        0,
    );
    report("mmap", t, now_us());
    let Ok(addr) = mapped else {
        println!("fbdemo: FAIL mmap");
        return 1;
    };

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let t = now_us();
    let vert = or_fail_gfx(backend.compile_shader(&SoftShaderDef::Vertex {
        func: color_vert,
        output_count: 5,
    }));
    report("compile_vertex", t, now_us());
    let t = now_us();
    let frag = or_fail_gfx(backend.compile_shader(&SoftShaderDef::Fragment(color_frag)));
    report("compile_fragment", t, now_us());
    let t = now_us();
    let pso = or_fail_gfx(backend.compile_pipeline(&PipelineDesc {
        vertex_shader: &vert,
        pixel_shader: &frag,
        blend: false,
        depth: false,
        vertex_stride: 20,
    }));
    report("compile_pipeline", t, now_us());

    // three vertices: red apex top, green bottom-left, blue bottom-right
    #[rustfmt::skip]
    let verts: [f32; 15] = [
         0.00,  0.75, 1.0, 0.0, 0.0,
        -0.75, -0.75, 0.0, 1.0, 0.0,
         0.75, -0.75, 0.0, 0.0, 1.0,
    ];
    let bytes: Vec<u8> = verts.iter().flat_map(|f| f.to_ne_bytes()).collect();

    let t = now_us();
    let mut vbuf = or_fail_gfx(backend.alloc_buffer(&BufferDesc {
        size: 60,
        is_dynamic: false,
    }));
    vbuf.data.copy_from_slice(&bytes);
    report("alloc_buffer", t, now_us());

    let stride = info.pitch as usize / 4;
    let width = info.width as usize;
    let height = info.height as usize;
    // SAFETY: the mmap covers `len` bytes and is page-aligned, hence u32-aligned.
    // `stride * (height - 1) + width` u32 words stay within that mapping.
    let fb_u32 = unsafe {
        core::slice::from_raw_parts_mut(addr.cast::<u32>(), stride * (height - 1) + width)
    };
    // The mapped framebuffer's initial content is undefined and `SoftQueue::new`
    // never clears its target, so uncovered pixels would keep whatever the device
    // left there.
    fb_u32.fill(0);

    let t = now_us();
    let mut q = SoftQueue::new(fb_u32, info.width, info.height, stride);
    report("queue_new", t, now_us());

    // render several frames to expose warm-up effects across submit/fsync
    for frame in 0..5u32 {
        let t0 = now_us();
        or_fail_gfx(q.submit(|rec| {
            rec.bind_pipeline(&pso);
            rec.bind_vertex_buffer(&vbuf);
            rec.draw(3);
        }));
        let t1 = now_us();
        let fsync_rc = fsync(fd);
        let t2 = now_us();
        if fsync_rc.is_err() {
            println!("fbdemo: FAIL fsync");
            return 1;
        }
        println!(
            "fbdemo: frame {frame} submit {}us fsync {}us",
            t1.saturating_sub(t0),
            t2.saturating_sub(t1)
        );
    }

    println!("fbdemo: frame drawn {}x{}", info.width, info.height);
    0
}
