#![no_std]
#![no_main]

use minilib::{ENOTTY, FbScreenInfo, IoctlRequest, O_RDONLY, ioctl, open, println};

minilib::entry!(main);

fn main() -> i32 {
    let Ok(fd) = open("/dev/fb0", O_RDONLY) else {
        println!("fb-ioctl: FAIL open");
        return 1;
    };

    let mut info = FbScreenInfo::default();
    if ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info).is_err() {
        println!("fb-ioctl: FAIL ioctl");
        return 1;
    }

    println!(
        "fb-ioctl: info {}x{} pitch={} bpp={}",
        info.width, info.height, info.pitch, info.bpp
    );

    // A regular file is not a tty, so the ioctl must fail with ENOTTY.
    let Ok(file_fd) = open("/bin/init", O_RDONLY) else {
        println!("fb-ioctl: FAIL enotty");
        return 1;
    };
    let mut enotty_info = FbScreenInfo::default();
    if ioctl(file_fd, IoctlRequest::FbGetScreenInfo, &mut enotty_info) == Err(ENOTTY) {
        println!("fb-ioctl: enotty ok");
    } else {
        println!("fb-ioctl: FAIL enotty");
        return 1;
    }

    0
}
