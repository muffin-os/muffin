#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::time::Duration;

use gfx::api::{CommandRecorder, GfxAllocator, GfxCompiler, GfxQueue, PipelineDesc};
use gfx::backend::software::{
    ATTRIBUTE_BYTES, SoftAllocator, SoftBackend, SoftCompiler, SoftQueue, SoftShaderDef,
};
use kernel_abi::gfx::BufferDesc;
use minilib::{
    ClockId, FbScreenInfo, IoctlRequest, MapFlags, ProtFlags, Timespec, clock_gettime, exit, fsync,
    ioctl, mmap, open, println,
};

fn now() -> Duration {
    let mut tp = Timespec::default();
    let Ok(now) = clock_gettime(ClockId::Monotonic, &mut tp).and_then(|()| Duration::try_from(tp))
    else {
        println!("fbdemo: FAIL clock");
        exit(1);
    };
    now
}

fn report(label: &str, start: Duration, end: Duration) {
    println!(
        "fbdemo: {label} {}us",
        end.saturating_sub(start).as_micros()
    );
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

// Vertex layout (x, y, r, g, b).
fn color_vert(input: &[f32], output: &mut [f32]) {
    output[0] = input[0];
    output[1] = input[1];
    output[2] = input[2];
    output[3] = input[3];
    output[4] = input[4];
}

/// Scales a unit-interval channel to 0..=255, saturating outside it.
// Runs three times per covered pixel. In an unoptimised build only
// `inline(always)` keeps it from becoming an out-of-line call.
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
    let t = now();
    let fd = open("/dev/fb0");
    report("open", t, now());
    let Ok(fd) = fd else {
        println!("fbdemo: FAIL open");
        return 1;
    };

    let mut info = FbScreenInfo::default();
    let t = now();
    let ioctl_rc = ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info);
    report("ioctl", t, now());
    if ioctl_rc.is_err() {
        println!("fbdemo: FAIL ioctl");
        return 1;
    }

    let len = info.len_bytes();
    let t = now();
    let mapped = mmap(
        0,
        len,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        fd,
        0,
    );
    report("mmap", t, now());
    let Ok(addr) = mapped else {
        println!("fbdemo: FAIL mmap");
        return 1;
    };

    let mut backend = SoftBackend(SoftAllocator, SoftCompiler);
    let t = now();
    let vert = or_fail_gfx(backend.compile_shader(&SoftShaderDef::Vertex {
        func: color_vert,
        output_count: 5,
    }));
    report("compile_vertex", t, now());
    let t = now();
    let frag = or_fail_gfx(backend.compile_shader(&SoftShaderDef::Fragment(color_frag)));
    report("compile_fragment", t, now());
    let t = now();
    let pso = or_fail_gfx(backend.compile_pipeline(&PipelineDesc {
        vertex_shader: &vert,
        pixel_shader: &frag,
        vertex_stride: 5 * ATTRIBUTE_BYTES,
    }));
    report("compile_pipeline", t, now());

    // three vertices: red apex top, green bottom-left, blue bottom-right
    #[rustfmt::skip]
    let verts: [f32; 15] = [
         0.00,  0.75, 1.0, 0.0, 0.0,
        -0.75, -0.75, 0.0, 1.0, 0.0,
         0.75, -0.75, 0.0, 0.0, 1.0,
    ];
    let bytes: Vec<u8> = verts.iter().flat_map(|f| f.to_ne_bytes()).collect();

    let t = now();
    let mut vbuf = or_fail_gfx(backend.alloc_buffer(&BufferDesc {
        size: bytes.len(),
        is_dynamic: false,
    }));
    vbuf.data.copy_from_slice(&bytes);
    report("alloc_buffer", t, now());

    // The software queue writes one u32 per pixel, so any other depth would
    // scribble over neighbouring pixels.
    let pixel_bytes = size_of::<u32>();
    let stride = info.pitch as usize / pixel_bytes;
    let width = info.width as usize;
    let height = info.height as usize;
    if info.bpp as usize != 8 * pixel_bytes
        || height == 0
        || width * pixel_bytes > info.pitch as usize
    {
        println!("fbdemo: FAIL geometry");
        return 1;
    }
    // SAFETY: the mmap covers `len` bytes and is page-aligned, hence u32-aligned.
    // The geometry check above keeps `stride * (height - 1) + width` u32 words
    // within `len = pitch * height` bytes.
    let fb_u32 = unsafe {
        core::slice::from_raw_parts_mut(addr.cast::<u32>(), stride * (height - 1) + width)
    };
    // The mapped framebuffer's initial content is undefined and `SoftQueue::new`
    // never clears its target, so uncovered pixels would keep whatever the device
    // left there.
    fb_u32.fill(0);

    let t = now();
    let mut q = SoftQueue::new(fb_u32, info.width, info.height, stride);
    report("queue_new", t, now());

    for frame in 0..5u32 {
        let t0 = now();
        or_fail_gfx(q.submit(|rec| {
            rec.bind_pipeline(&pso);
            rec.bind_vertex_buffer(&vbuf);
            rec.draw(3);
        }));
        let t1 = now();
        let fsync_rc = fsync(fd);
        let t2 = now();
        if fsync_rc.is_err() {
            println!("fbdemo: FAIL fsync");
            return 1;
        }
        println!(
            "fbdemo: frame {frame} submit {}us fsync {}us",
            t1.saturating_sub(t0).as_micros(),
            t2.saturating_sub(t1).as_micros()
        );
    }

    println!("fbdemo: frame drawn {}x{}", info.width, info.height);
    0
}
