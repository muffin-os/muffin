use kernel_abi::{EBADF, EINVAL, ENOMEM, Errno, MapFlags, ProtFlags};
use tracing::{Level, instrument};

use crate::UserspacePtr;
use crate::access::{AllocationStrategy, Location, MemoryRegionAccess};

#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_mmap<Cx: MemoryRegionAccess>(
    cx: &Cx,
    addr: UserspacePtr<u8>,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: usize,
) -> Result<usize, Errno> {
    if len == 0 {
        return Err(EINVAL);
    }

    let flags = MapFlags::from_bits(flags).ok_or(EINVAL)?;

    let prot = ProtFlags::from_bits(prot).ok_or(EINVAL)?;

    // ensure WRITE and EXEC are mutually exclusive (W^X policy)
    if prot.contains(ProtFlags::WRITE) && prot.contains(ProtFlags::EXEC) {
        return Err(EINVAL);
    }

    if flags.contains(MapFlags::ANONYMOUS) {
        if !flags.contains(MapFlags::PRIVATE) {
            return Err(EINVAL);
        }

        let location = if flags.contains(MapFlags::FIXED) {
            // when MAP_FIXED is set, addr must not be null
            if addr.as_ptr().is_null() {
                return Err(EINVAL);
            }
            // validate that addr and addr+len are in lower half
            addr.validate_range(len)?;
            Location::Fixed(addr)
        } else {
            // when MAP_FIXED is not set, addr is just a hint and is ignored
            Location::Anywhere
        };

        let mapped_addr = cx
            .create_and_track_mapping(location, len, AllocationStrategy::Eager)
            .map_err(|e| match e {
                crate::access::CreateMappingError::LocationAlreadyMapped => EINVAL,
                crate::access::CreateMappingError::OutOfMemory => ENOMEM,
            })?;

        Ok(mapped_addr.addr())
    } else {
        // A non-anonymous mapping is backed by an open file descriptor and
        // must be shared so writes reach the device frames.
        if !flags.contains(MapFlags::SHARED) {
            return Err(EINVAL);
        }
        if fd < 0 {
            return Err(EBADF);
        }
        if offset != 0 {
            return Err(EINVAL);
        }
        // MAP_FIXED is not supported for file-backed mappings; the kernel
        // chooses the address.
        if flags.contains(MapFlags::FIXED) {
            return Err(EINVAL);
        }

        let mapped_addr = cx.map_shared_file(fd, len)?;
        Ok(mapped_addr.addr())
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use kernel_abi::{EINVAL, ENODEV, MapFlags, ProtFlags};
    use spin::mutex::Mutex;

    use crate::UserspacePtr;
    use crate::access::{
        AllocationStrategy, CreateMappingError, Location, MemoryRegion, MemoryRegionAccess,
    };
    use crate::mman::sys_mmap;

    struct TestRegion {
        addr: UserspacePtr<u8>,
        size: usize,
    }

    impl MemoryRegion for TestRegion {
        fn addr(&self) -> UserspacePtr<u8> {
            self.addr
        }

        fn size(&self) -> usize {
            self.size
        }
    }

    struct TestMemoryAccess {
        mappings: Mutex<Vec<(usize, usize)>>, // (addr, size)
        next_addr: Mutex<usize>,
    }

    impl TestMemoryAccess {
        fn new() -> Self {
            Self {
                mappings: Mutex::new(Vec::new()),
                next_addr: Mutex::new(0x1000), // Start at page boundary
            }
        }
    }

    impl MemoryRegionAccess for Arc<TestMemoryAccess> {
        type Region = TestRegion;

        fn create_and_track_mapping(
            &self,
            location: Location,
            size: usize,
            _allocation_strategy: AllocationStrategy,
        ) -> Result<UserspacePtr<u8>, CreateMappingError> {
            let addr = match location {
                Location::Anywhere => {
                    let mut next = self.next_addr.lock();
                    let addr = *next;
                    *next += size;
                    addr
                }
                Location::Fixed(ptr) => {
                    let addr = ptr.addr();
                    // Check if this overlaps with existing mappings
                    let mappings = self.mappings.lock();
                    for (existing_addr, existing_size) in mappings.iter() {
                        if addr < existing_addr + existing_size && existing_addr < &(addr + size) {
                            return Err(CreateMappingError::LocationAlreadyMapped);
                        }
                    }
                    addr
                }
            };

            let ptr = unsafe { UserspacePtr::try_from_usize(addr).unwrap() };

            self.mappings.lock().push((addr, size));

            let region = TestRegion { addr: ptr, size };
            self.add_memory_region(region);
            Ok(ptr)
        }

        fn add_memory_region(&self, _region: Self::Region) {
            // Just a placeholder for testing
        }
    }

    #[test]
    fn test_mmap_anonymous_private() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = unsafe { UserspacePtr::try_from_usize(0).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            (ProtFlags::READ | ProtFlags::WRITE).bits(),
            (MapFlags::ANONYMOUS | MapFlags::PRIVATE).bits(),
            0,
            0,
        );

        assert!(result.is_ok());
        let mapped_addr = result.unwrap();
        assert!(mapped_addr != 0);
        assert!(mapped_addr < (1_usize << 63)); // Lower half
    }

    #[test]
    fn test_mmap_zero_size() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = unsafe { UserspacePtr::try_from_usize(0).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            0,
            (ProtFlags::READ | ProtFlags::WRITE).bits(),
            (MapFlags::ANONYMOUS | MapFlags::PRIVATE).bits(),
            0,
            0,
        );

        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn test_mmap_not_anonymous() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = unsafe { UserspacePtr::try_from_usize(0).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            (ProtFlags::READ | ProtFlags::WRITE).bits(),
            MapFlags::PRIVATE.bits(), // Missing MAP_ANONYMOUS
            0,
            0,
        );

        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn test_mmap_not_private() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = unsafe { UserspacePtr::try_from_usize(0).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            (ProtFlags::READ | ProtFlags::WRITE).bits(),
            MapFlags::ANONYMOUS.bits(), // Missing MAP_PRIVATE
            0,
            0,
        );

        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn test_mmap_fixed() {
        let cx = Arc::new(TestMemoryAccess::new());
        let fixed_addr = 0x100000;
        let addr = unsafe { UserspacePtr::try_from_usize(fixed_addr).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            (ProtFlags::READ | ProtFlags::WRITE).bits(),
            (MapFlags::ANONYMOUS | MapFlags::PRIVATE | MapFlags::FIXED).bits(),
            0,
            0,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), fixed_addr);
    }

    #[test]
    fn test_mmap_write_exec_mutually_exclusive() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = unsafe { UserspacePtr::try_from_usize(0).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            (ProtFlags::WRITE | ProtFlags::EXEC).bits(), // Both WRITE and EXEC
            (MapFlags::ANONYMOUS | MapFlags::PRIVATE).bits(),
            0,
            0,
        );

        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn mmap_non_anonymous_requires_shared() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = unsafe { UserspacePtr::try_from_usize(0).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ.bits(),
            MapFlags::PRIVATE.bits(), // neither ANONYMOUS nor SHARED
            3,
            0,
        );

        assert_eq!(
            result,
            Err(EINVAL),
            "non-anonymous mapping without MAP_SHARED must be rejected"
        );
    }

    #[test]
    fn mmap_shared_fd_unsupported() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = unsafe { UserspacePtr::try_from_usize(0).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ.bits(),
            MapFlags::SHARED.bits(),
            3,
            0,
        );

        assert_eq!(
            result,
            Err(ENODEV),
            "shared fd mapping must hit the default map_shared_file rejecting with ENODEV"
        );
    }

    #[test]
    fn mmap_shared_offset_rejected() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = unsafe { UserspacePtr::try_from_usize(0).unwrap() };

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ.bits(),
            MapFlags::SHARED.bits(),
            3,
            4096, // non-zero offset is unsupported
        );

        assert_eq!(
            result,
            Err(EINVAL),
            "shared fd mapping with a non-zero offset must be rejected"
        );
    }
}
