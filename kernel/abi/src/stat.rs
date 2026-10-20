/// File metadata copied out to userspace by the fstat syscall.
///
/// This is the ring 3 wire layout. A field may only be appended, and ring 0 and
/// ring 3 must be rebuilt together when one is.
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct Stat {
    pub size: u64,
}
