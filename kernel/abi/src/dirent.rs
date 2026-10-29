/// Header of one record the getdents syscall writes. `name_len` bytes of
/// name follow the header. The next record starts right after the name.
///
/// This is the ring 3 wire layout. A field may only be appended, and ring 0
/// and ring 3 must be rebuilt together when one is.
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct DirentHeader {
    pub name_len: u8,
}
