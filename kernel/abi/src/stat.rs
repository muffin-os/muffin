/// File type and permission bits of [`Stat::mode`], with the POSIX values so
/// userspace passes them through to `st_mode`.
pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFBLK: u32 = 0o060000;

/// File metadata copied out to userspace by the fstat syscall.
///
/// This is the ring 3 wire layout. A field may only be appended, and ring 0 and
/// ring 3 must be rebuilt together when one is.
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct Stat {
    pub size: u64,
    /// POSIX `st_mode`: one of the `S_IF*` type bits plus permission bits.
    pub mode: u32,
}
