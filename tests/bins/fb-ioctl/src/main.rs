#![no_std]
#![no_main]

use minilib::{FbScreenInfo, IoctlRequest, exit, ioctl, open, write};

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

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    let fd = open("/dev/fb0");
    if fd < 0 {
        puts("fb-ioctl: FAIL open\n");
        exit(1);
    }

    let mut info = FbScreenInfo::default();
    if ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut info) != 0 {
        puts("fb-ioctl: FAIL ioctl\n");
        exit(1);
    }

    puts("fb-ioctl: info ");
    put_u32(info.width);
    puts("x");
    put_u32(info.height);
    puts(" pitch=");
    put_u32(info.pitch);
    puts(" bpp=");
    put_u32(info.bpp);
    puts("\n");

    let spawn_fd = open("/spawn");
    if spawn_fd < 0 {
        puts("fb-ioctl: FAIL enotty\n");
        exit(1);
    }
    let mut enotty_info = FbScreenInfo::default();
    if ioctl(spawn_fd, IoctlRequest::FbGetScreenInfo, &mut enotty_info) == -58 {
        puts("fb-ioctl: enotty ok\n");
    } else {
        puts("fb-ioctl: FAIL enotty\n");
        exit(1);
    }

    exit(0);
}
