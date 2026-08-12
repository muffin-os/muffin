#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use gfx::api::{CommandRecorder, GfxAllocator, GfxCompiler, GfxQueue, PipelineDesc};
use gfx::backend::software::{SoftAllocator, SoftBackend, SoftCompiler, SoftQueue, SoftShaderDef};
use kernel_abi::gfx::BufferDesc;
use minilib::{
    CLOCK_MONOTONIC, FbScreenInfo, IoctlRequest, MapFlags, ProtFlags, Timespec, clock_gettime,
    exit, fsync, ioctl, mmap, open, write,
};

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
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
    let _ = write(1, &buf[i..]);
}

fn put_u64(n: u64) {
    let mut buf = [0u8; 20];
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
    let _ = write(1, &buf[i..]);
}

// monotonic timestamp in microseconds; on failure the zeroed timespec yields 0
fn now_us() -> u64 {
    let mut tp = Timespec::default();
    let _ = clock_gettime(CLOCK_MONOTONIC, &mut tp);
    (tp.tv_sec as u64) * 1_000_000 + (tp.tv_nsec as u64) / 1000
}

fn report(label: &str, start_us: u64, end_us: u64) {
    puts("fbdemo: ");
    puts(label);
    puts(" ");
    put_u64(end_us.saturating_sub(start_us));
    puts("us\n");
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
        puts("fbdemo: FAIL open\n");
        exit(1);
    };

    let mut info = FbScreenInfo::default();
    let t = now_us();
    let ioctl_rc = ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info);
    report("ioctl", t, now_us());
    if ioctl_rc.is_err() {
        puts("fbdemo: FAIL ioctl\n");
        exit(1);
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
        puts("fbdemo: FAIL mmap\n");
        exit(1);
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
            puts("fbdemo: FAIL fsync\n");
            exit(1);
        }
        puts("fbdemo: frame ");
        put_u32(frame);
        puts(" submit ");
        put_u64(t1.saturating_sub(t0));
        puts("us fsync ");
        put_u64(t2.saturating_sub(t1));
        puts("us\n");
    }

    puts("fbdemo: frame drawn ");
    put_u32(info.width);
    puts("x");
    put_u32(info.height);
    puts("\n");
    exit(0)
}
