use kernel_abi::{Errno, EINVAL, ENOMEM, MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, PROT_EXEC, PROT_READ, PROT_WRITE};

use crate::UserspacePtr;
use crate::access::{AllocationStrategy, FileAccess, Location, MemoryAccess, MemoryRegionAccess};

pub fn sys_mmap<Cx: FileAccess + MemoryAccess + MemoryRegionAccess>(
    cx: &Cx,
    addr: UserspacePtr<u8>,
    len: usize,
    prot: i32,
    flags: i32,
    fd: Cx::Fd,
    offset: usize,
) -> Result<usize, Errno> {
    // Validate size is non-zero
    if len == 0 {
        return Err(EINVAL);
    }

    // For now, only support anonymous private mappings
    if flags & MAP_ANONYMOUS == 0 {
        return Err(EINVAL);
    }
    if flags & MAP_PRIVATE == 0 {
        return Err(EINVAL);
    }

    // Validate protection flags
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(EINVAL);
    }

    // Determine location
    let location = if addr.as_ptr().is_null() {
        Location::Anywhere
    } else {
        // Validate that addr and addr+len are in lower half
        unsafe {
            addr.validate_range(len)?;
        }
        
        if flags & MAP_FIXED != 0 {
            Location::Fixed(addr)
        } else {
            // When MAP_FIXED is not set, addr is just a hint
            // For simplicity, we'll treat it as Anywhere
            Location::Anywhere
        }
    };

    // We'll use eager allocation for now (as specified in requirements)
    let allocation_strategy = AllocationStrategy::Eager;

    // Create the mapping and add it to the process's memory regions
    // The context is responsible for converting the mapping to a region
    cx.create_and_track_mapping(location, len, allocation_strategy)
        .map_err(|e| match e {
            crate::access::CreateMappingError::LocationAlreadyMapped => EINVAL,
            crate::access::CreateMappingError::OutOfMemory => ENOMEM,
        })
        .map(|addr| addr.addr())
        .map(|addr| {
            // Suppress unused parameter warnings for fd, offset, and prot (not used for anonymous mappings)
            let _ = (fd, offset, prot);
            addr
        })
}
