#![no_std]
#![no_main]

use minilib::{
    FbScreenInfo, IoctlRequest, MapFlags, ProtFlags, exit, ioctl, mmap, open, read, write,
};

const PATTERN_LEN: usize = 256;

fn puts(msg: &str) {
    write(1, msg.as_bytes());
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    let fd = open("/dev/fb0");
    if fd < 0 {
        puts("fb-mmap: FAIL open\n");
        exit(1);
    }

    let mut info = FbScreenInfo::default();
    if ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info) != 0 {
        puts("fb-mmap: FAIL ioctl\n");
        exit(1);
    }

    let len = info.pitch as usize * info.height as usize;
    let base = mmap(
        0,
        len,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        fd as usize,
        0,
    );
    if base <= 0 {
        puts("fb-mmap: FAIL mmap\n");
        exit(1);
    }

    // Stamp a recognizable pattern directly through the user mapping. If the
    // mapping aliases the real device memory, reading the same offsets back
    // through the read syscall must observe these exact bytes.
    let ptr = base as *mut u8;
    for i in 0..PATTERN_LEN {
        unsafe {
            ptr.add(i).write_volatile((i as u8) ^ 0x5A);
        }
    }

    let mut buf = [0u8; PATTERN_LEN];
    if read(fd, &mut buf) != PATTERN_LEN as i32 {
        puts("fb-mmap: FAIL readback\n");
        exit(1);
    }
    for (i, &item) in buf.iter().enumerate().take(PATTERN_LEN) {
        if item != (i as u8) ^ 0x5A {
            puts("fb-mmap: FAIL readback\n");
            exit(1);
        }
    }

    // Shared mapping of a regular ext2 file is unsupported (FileSystem::mmap
    // default rejects it), so the syscall must fail.
    let spawn_fd = open("/spawn");
    if mmap(
        0,
        4096,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        spawn_fd as usize,
        0,
    ) >= 0
    {
        puts("fb-mmap: FAIL shared-regular\n");
        exit(1);
    }

    // file-backed PRIVATE mapping is likewise unsupported
    if mmap(
        0,
        4096,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::PRIVATE,
        fd as usize,
        0,
    ) >= 0
    {
        puts("fb-mmap: FAIL private-file\n");
        exit(1);
    }

    puts("fb-mmap: ok\n");
    exit(0);
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &::core::panic::PanicInfo) -> ! {
    loop {}
}
