/// File metadata copied out to userspace by the fstat syscall.
///
/// This is the ring 3 wire layout. A field may only be appended, and ring 0 and
/// ring 3 must be rebuilt together when one is.
///
/// `kernel_vfs::Stat` is the only source the kernel fills this from and it
/// holds nothing but the size. Another field has to reach that struct, and
/// every filesystem behind it, before it can appear here.
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct Stat {
    pub size: u64,
}
