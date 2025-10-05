use kernel_abi::Errno;

use crate::UserspacePtr;
use crate::access::{AllocationStrategy, FileAccess, Location, Mapping, MemoryAccess};

pub fn sys_mmap<Cx: FileAccess + MemoryAccess>(
    cx: &Cx,
    addr: UserspacePtr<u8>,
    len: usize,
    prot: i32,
    flags: i32,
    fd: Cx::Fd,
    offset: usize,
) -> Result<usize, Errno> {
    let _ = (cx, addr, len, prot, flags, fd, offset);

    let location = if addr.as_ptr().is_null() {
        Location::Anywhere
    } else {
        Location::Fixed(addr)
    };

    todo!()
}
