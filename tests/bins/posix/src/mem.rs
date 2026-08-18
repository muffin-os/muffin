use minilib::{
    EBADF, EINVAL, ENOMEM, EOVERFLOW, Errno, MapFlags, ProtFlags, SYS_MMAP, mmap, ret, syscall6,
};

use crate::check;

const PAGE: usize = 4096;

fn raw_mmap(
    addr: usize,
    len: usize,
    prot: usize,
    flags: usize,
    fd: usize,
    offset: usize,
) -> Result<usize, Errno> {
    ret(syscall6(SYS_MMAP, addr, len, prot, flags, fd, offset))
}

fn fill_and_verify(name: &str, base: *mut u8, len: usize) {
    for i in 0..len {
        // Safety: the mapping covers len writable bytes starting at base.
        unsafe { core::ptr::write_volatile(base.add(i), (i as u8) ^ 0x5A) };
    }
    for i in 0..len {
        // Safety: same mapping, reading back the bytes just written.
        let got = unsafe { core::ptr::read_volatile(base.add(i)) };
        check::require(name, got == (i as u8) ^ 0x5A);
    }
}

pub fn run() {
    check::group("mmap");

    let rw = ProtFlags::READ | ProtFlags::WRITE;
    let anon_private = MapFlags::ANONYMOUS | MapFlags::PRIVATE;

    let base = check::unwrap_or_fail("mmap/anon_page", mmap(0, PAGE, rw, anon_private, 0, 0));
    check::require(
        "mmap/anon_page_aligned",
        (base as usize).is_multiple_of(PAGE),
    );
    fill_and_verify("mmap/anon_page_readback", base, PAGE);

    let partial = check::unwrap_or_fail(
        "mmap/unaligned_len",
        mmap(0, PAGE / 4 + 1, rw, anon_private, 0, 0),
    );
    check::require(
        "mmap/unaligned_len_aligned",
        (partial as usize).is_multiple_of(PAGE),
    );
    fill_and_verify("mmap/unaligned_len_rounded_up", partial, PAGE);

    check::expect_err("mmap/zero_len", mmap(0, 0, rw, anon_private, 0, 0), EINVAL);
    check::expect_err(
        "mmap/anon_without_private",
        mmap(0, PAGE, rw, MapFlags::ANONYMOUS, 0, 0),
        EINVAL,
    );
    check::expect_err(
        "mmap/anon_fixed_null_addr",
        mmap(0, PAGE, rw, anon_private | MapFlags::FIXED, 0, 0),
        EINVAL,
    );
    check::expect_err(
        "mmap/write_exec_rejected",
        mmap(
            0,
            PAGE,
            ProtFlags::WRITE | ProtFlags::EXEC,
            anon_private,
            0,
            0,
        ),
        EINVAL,
    );
    check::expect_err(
        "mmap/huge_len",
        mmap(0, 1 << 46, rw, anon_private, 0, 0),
        ENOMEM,
    );

    check::expect_err(
        "mmap/file_no_sharing_mode",
        mmap(0, PAGE, rw, MapFlags::empty(), 0, 0),
        EINVAL,
    );
    check::expect_err(
        "mmap/file_both_sharing_modes",
        mmap(0, PAGE, rw, MapFlags::SHARED | MapFlags::PRIVATE, 0, 0),
        EINVAL,
    );
    check::expect_err(
        "mmap/file_fixed_rejected",
        mmap(0, PAGE, rw, MapFlags::PRIVATE | MapFlags::FIXED, 0, 0),
        EINVAL,
    );
    check::expect_err(
        "mmap/file_unopened_fd",
        mmap(0, PAGE, rw, MapFlags::PRIVATE, 100, 0),
        EBADF,
    );

    let rw_bits = rw.bits() as usize;
    let anon_private_bits = anon_private.bits() as usize;
    check::expect_err(
        "mmap/unknown_prot_bit",
        raw_mmap(0, PAGE, 0x8, anon_private_bits, 0, 0),
        EINVAL,
    );
    check::expect_err(
        "mmap/unknown_flag_bit",
        raw_mmap(0, PAGE, rw_bits, 0x40, 0, 0),
        EINVAL,
    );
    check::expect_err(
        "mmap/prot_above_i32_max",
        raw_mmap(0, PAGE, 1 << 31, anon_private_bits, 0, 0),
        EOVERFLOW,
    );
    check::expect_err(
        "mmap/flags_above_i32_max",
        raw_mmap(0, PAGE, rw_bits, 1 << 31, 0, 0),
        EOVERFLOW,
    );
    check::expect_err(
        "mmap/fd_above_i32_max",
        raw_mmap(0, PAGE, rw_bits, anon_private_bits, 1 << 31, 0),
        EOVERFLOW,
    );
}
