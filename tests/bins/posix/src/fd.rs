use minilib::{
    EBADF, EFAULT, EINVAL, EISDIR, ENOENT, ENOSYS, ENOTTY, FbScreenInfo, IoctlRequest, SYS_CLOSE,
    SYS_IOCTL, SYS_LSEEK, SYS_READ, SYS_WRITE, Stat, Whence, fstat, fsync, ioctl, lseek, open,
    read, ret, write,
};

use crate::check;

pub(crate) const HELLO: &[u8] = b"muffin says hi\n";

const UNOPENED_FD: i32 = 4095;

const OVERSIZED_FD: usize = usize::MAX;

const KERNEL_ADDR: usize = 0xffff_8000_0000_0000;

const NONCANONICAL_ADDR: usize = 0x0001_0000_0000_0000;

pub fn run() {
    check::group("fd");

    check::expect_err("fd/open_missing", open("/data/nope"), ENOENT);

    let fd = check::unwrap_or_fail("fd/open_hello", open("/data/hello.txt"));
    check::require("fd/open_lowest_free", fd == 3);

    let mut buf = [0_u8; HELLO.len()];
    check::expect_ok("fd/read_full", read(fd, &mut buf), HELLO.len());
    check::require("fd/read_contents", &buf[..] == HELLO);

    let mut one = [0_u8; 1];
    check::expect_ok("fd/read_at_eof", read(fd, &mut one), 0);

    let mut stat = Stat::default();
    check::expect_ok("fd/fstat_hello", fstat(fd, &mut stat), ());
    check::require("fd/fstat_size", stat.size == 15);

    check::expect_ok("fd/fsync_hello", fsync(fd), ());

    check::expect_ok("fd/lseek_set", lseek(fd, 10, Whence::Set), 10);
    check::expect_ok("fd/lseek_cur", lseek(fd, 3, Whence::Cur), 13);
    check::expect_ok("fd/lseek_end", lseek(fd, 0, Whence::End), 15);
    check::expect_ok("fd/lseek_end_back", lseek(fd, -5, Whence::End), 10);

    let mut tail = [0_u8; 5];
    check::expect_ok("fd/read_after_lseek", read(fd, &mut tail), 5);
    check::require("fd/read_after_lseek_contents", &tail == b"s hi\n");

    check::expect_err("fd/lseek_negative", lseek(fd, -1, Whence::Set), EINVAL);
    check::expect_err(
        "fd/lseek_bad_whence",
        ret(unsafe { minilib::syscall3(SYS_LSEEK, fd as usize, 0, 3) }),
        EINVAL,
    );

    let mut screen = FbScreenInfo::default();
    check::expect_err(
        "fd/ioctl_regular_file",
        ioctl(fd, IoctlRequest::FbGetScreenInfo, &mut screen),
        ENOTTY,
    );
    check::expect_err(
        "fd/ioctl_unknown_request",
        ret(unsafe {
            minilib::syscall3(
                SYS_IOCTL,
                fd as usize,
                2,
                core::ptr::from_mut(&mut screen) as usize,
            )
        }),
        ENOTTY,
    );

    check::expect_err("fd/read_unopened", read(UNOPENED_FD, &mut one), EBADF);
    check::expect_err("fd/write_unopened", write(UNOPENED_FD, HELLO), EBADF);
    check::expect_err("fd/fsync_unopened", fsync(UNOPENED_FD), EBADF);
    check::expect_err("fd/fstat_unopened", fstat(UNOPENED_FD, &mut stat), EBADF);
    check::expect_err(
        "fd/lseek_set_unopened",
        lseek(UNOPENED_FD, 0, Whence::Set),
        EBADF,
    );
    check::expect_err(
        "fd/lseek_cur_unopened",
        lseek(UNOPENED_FD, 0, Whence::Cur),
        EBADF,
    );
    check::expect_err(
        "fd/lseek_end_unopened",
        lseek(UNOPENED_FD, 0, Whence::End),
        EBADF,
    );

    check::expect_err(
        "fd/read_fd_above_i32",
        ret(unsafe {
            minilib::syscall3(SYS_READ, OVERSIZED_FD, one.as_mut_ptr() as usize, one.len())
        }),
        EINVAL,
    );
    check::expect_err(
        "fd/write_fd_above_i32",
        ret(unsafe {
            minilib::syscall3(
                SYS_WRITE,
                OVERSIZED_FD,
                HELLO.as_ptr() as usize,
                HELLO.len(),
            )
        }),
        EINVAL,
    );

    check::expect_err("fd/read_zero_len", read(fd, &mut []), EINVAL);
    check::expect_err("fd/write_zero_len", write(1, &[]), EINVAL);

    // Safety: these buffers are deliberately invalid, and the kernel rejecting
    // them before any access is what each check asserts.
    check::expect_err(
        "fd/read_null_buf",
        ret(unsafe { minilib::syscall3(SYS_READ, fd as usize, 0, one.len()) }),
        EINVAL,
    );
    check::expect_err(
        "fd/read_kernel_buf",
        ret(unsafe { minilib::syscall3(SYS_READ, fd as usize, KERNEL_ADDR, 8) }),
        EFAULT,
    );
    check::expect_err(
        "fd/read_noncanonical_buf",
        ret(unsafe { minilib::syscall3(SYS_READ, fd as usize, NONCANONICAL_ADDR, 8) }),
        EFAULT,
    );
    check::expect_err(
        "fd/write_kernel_buf",
        ret(unsafe { minilib::syscall3(SYS_WRITE, fd as usize, KERNEL_ADDR, 8) }),
        EFAULT,
    );

    let dir = check::unwrap_or_fail("fd/open_dir", open("/data/dir"));
    check::require("fd/open_dir_next_free", dir == fd + 1);
    check::expect_err("fd/read_dir", read(dir, &mut one), EISDIR);
    check::expect_ok("fd/fstat_dir", fstat(dir, &mut stat), ());
    check::require("fd/fstat_dir_size", stat.size == 1024);

    let via_dotdot = check::unwrap_or_fail("fd/open_dotdot", open("/data/dir/../hello.txt"));
    check::expect_ok("fd/fstat_dotdot", fstat(via_dotdot, &mut stat), ());
    check::require("fd/fstat_dotdot_size", stat.size == 15);

    check::expect_err("fd/open_through_file", open("/data/hello.txt/x"), ENOENT);

    check::expect_err(
        "fd/close_unimplemented",
        ret(unsafe { minilib::syscall1(SYS_CLOSE, fd as usize) }),
        ENOSYS,
    );
    let null = check::unwrap_or_fail("fd/open_dev_null", open("/dev/null"));
    check::expect_ok("fd/read_dev_null", read(null, &mut one), 0);
    check::expect_ok("fd/write_dev_null", write(null, HELLO), HELLO.len());
}
