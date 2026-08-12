#![no_std]
#![no_main]

use minilib::{
    FbScreenInfo, IoctlRequest, MapFlags, ProtFlags, Whence, exit, ioctl, lseek, mmap, open, read,
    write,
};

const PATTERN_LEN: usize = 256;

fn puts(msg: &str) {
    let _ = write(1, msg.as_bytes());
}

minilib::entry!(main);

fn main() -> i32 {
    let Ok(fd) = open("/dev/fb0") else {
        puts("fb-mmap: FAIL open\n");
        exit(1);
    };

    let mut info = FbScreenInfo::default();
    if ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info).is_err() {
        puts("fb-mmap: FAIL ioctl\n");
        exit(1);
    }

    let len = info.pitch as usize * info.height as usize;
    let Ok(ptr) = mmap(
        0,
        len,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        fd as usize,
        0,
    ) else {
        puts("fb-mmap: FAIL mmap\n");
        exit(1);
    };

    // Stamp a recognizable pattern directly through the user mapping. If the
    // mapping aliases the real device memory, reading the same offsets back
    // through the read syscall must observe these exact bytes.
    for i in 0..PATTERN_LEN {
        unsafe {
            ptr.add(i).write_volatile((i as u8) ^ 0x5A);
        }
    }

    let mut buf = [0u8; PATTERN_LEN];
    if read(fd, &mut buf) != Ok(PATTERN_LEN) {
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
    let Ok(spawn_fd) = open("/spawn") else {
        puts("fb-mmap: FAIL shared-regular\n");
        exit(1);
    };
    if mmap(
        0,
        4096,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        spawn_fd as usize,
        0,
    )
    .is_ok()
    {
        puts("fb-mmap: FAIL shared-regular\n");
        exit(1);
    }

    // A private mapping is served by a lazy page-in that reads the device, so
    // the pattern held in the framebuffer must be visible through it.
    let Ok(private_ptr) = mmap(
        0,
        4096,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::PRIVATE,
        fd as usize,
        0,
    ) else {
        puts("fb-mmap: FAIL private-file\n");
        exit(1);
    };
    for i in 0..PATTERN_LEN {
        if unsafe { private_ptr.add(i).read_volatile() } != (i as u8) ^ 0x5A {
            puts("fb-mmap: FAIL private-file\n");
            exit(1);
        }
    }

    // A private mapping owns its frames and is never written back, so the
    // device keeps the pattern byte.
    unsafe {
        private_ptr.write_volatile(0xFF);
    }
    let mut first = [0u8; 1];
    if lseek(fd, 0, Whence::Set) != Ok(0) || read(fd, &mut first) != Ok(1) || first[0] != 0x5A {
        puts("fb-mmap: FAIL private-writeback\n");
        exit(1);
    }

    puts("fb-mmap: ok\n");
    exit(0)
}
