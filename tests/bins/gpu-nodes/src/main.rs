//! Prints `gpu-nodes: ok`; the first failure prints
//! `gpu-nodes: FAIL <step> <detail>` and exits 1.

use std::ffi::{c_char, c_int, c_uint};
use std::process::exit;

use kernel_abi::{FdNum, GpuFlush, GpuImport, GpuInfo, GpuResourceKind, GpuSetScanout};
use muffin_sys::{EBUSY, ENOTTY, Errno, IoctlRequest, O_CREAT, O_EXCL, O_RDWR};

unsafe extern "C" {
    fn shm_open(name: *const c_char, oflag: c_int, mode: c_uint) -> c_int;
}

fn fail(step: &str, detail: impl std::fmt::Debug) -> ! {
    println!("gpu-nodes: FAIL {step} {detail:?}");
    exit(1);
}

fn ok<T>(step: &str, result: Result<T, Errno>) -> T {
    result.unwrap_or_else(|e| fail(step, e))
}

fn expect_err<T: std::fmt::Debug>(step: &str, result: Result<T, Errno>, expected: Errno) {
    match result {
        Err(e) if e == expected => {}
        other => fail(step, other),
    }
}

fn import(fd: c_int, shm: c_int, width: u32, height: u32) -> Result<u32, Errno> {
    let mut arg = GpuImport {
        fd: FdNum::from(shm),
        kind: GpuResourceKind::Texture,
        width,
        height,
        resource: 0,
    };
    muffin_sys::ioctl(fd, IoctlRequest::GpuImport, &mut arg)?;
    Ok(arg.resource)
}

fn set_scanout(fd: c_int, resource: u32) -> Result<usize, Errno> {
    muffin_sys::ioctl(
        fd,
        IoctlRequest::GpuSetScanout,
        &mut GpuSetScanout { resource },
    )
}

fn flush(fd: c_int, resource: u32, width: u32, height: u32) -> Result<usize, Errno> {
    let mut arg = GpuFlush {
        resource,
        x: 0,
        y: 0,
        width,
        height,
    };
    muffin_sys::ioctl(fd, IoctlRequest::GpuFlush, &mut arg)
}

fn main() {
    let render = ok("info", muffin_sys::open("/dev/render0", O_RDWR));
    let mut info = GpuInfo::default();
    ok(
        "info",
        muffin_sys::ioctl(render, IoctlRequest::GpuGetInfo, &mut info),
    );
    if info.has_3d != 1 || info.width == 0 || info.height == 0 {
        fail("info", info);
    }
    let (width, height) = (info.width, info.height);

    let shm = unsafe { shm_open(c"/gpu-nodes".as_ptr(), O_RDWR | O_CREAT | O_EXCL, 0o600) };
    if shm < 0 {
        fail("object", std::io::Error::last_os_error());
    }
    ok(
        "object",
        muffin_sys::ftruncate(shm, i64::from(width) * i64::from(height) * 4),
    );

    let resource = ok("render-import", import(render, shm, width, height));
    expect_err("render-scanout", set_scanout(render, resource), ENOTTY);
    expect_err(
        "render-flush",
        flush(render, resource, width, height),
        ENOTTY,
    );

    let a = ok("display-a", muffin_sys::open("/dev/gpu0", O_RDWR));
    let id_a = ok("display-a", import(a, shm, width, height));
    if id_a != resource {
        fail("display-a", (id_a, resource));
    }
    let b = ok("display-b", muffin_sys::open("/dev/gpu0", O_RDWR));
    let id_b = ok("display-b", import(b, shm, width, height));
    if id_b != resource {
        fail("display-b", (id_b, resource));
    }

    ok("owner", set_scanout(a, resource));
    expect_err("other-scanout", set_scanout(b, resource), EBUSY);
    expect_err("other-flush", flush(b, resource, width, height), EBUSY);
    ok("owner-flush", flush(a, resource, width, height));

    ok("release", muffin_sys::close(a));
    ok("release", set_scanout(b, resource));

    println!("gpu-nodes: ok");
}
