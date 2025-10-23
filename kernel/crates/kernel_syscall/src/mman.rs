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

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::ffi::c_int;

    use kernel_abi::{EINVAL, MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, PROT_READ, PROT_WRITE};
    use kernel_vfs::path::AbsolutePath;
    use spin::mutex::Mutex;

    use crate::UserspacePtr;
    use crate::access::{
        AllocationStrategy, CreateMappingError, FileAccess, FileInfo, Location, Mapping,
        MemoryAccess, MemoryRegion, MemoryRegionAccess,
    };
    use crate::mman::sys_mmap;

    struct TestMapping {
        addr: UserspacePtr<u8>,
        size: usize,
    }

    impl Mapping for TestMapping {
        fn addr(&self) -> UserspacePtr<u8> {
            self.addr
        }

        fn size(&self) -> usize {
            self.size
        }
    }

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

    struct TestFileInfo;
    impl FileInfo for TestFileInfo {}

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

    impl FileAccess for Arc<TestMemoryAccess> {
        type FileInfo = TestFileInfo;
        type Fd = c_int;
        type OpenError = ();
        type ReadError = ();
        type WriteError = ();
        type CloseError = ();

        fn file_info(&self, _path: &AbsolutePath) -> Option<Self::FileInfo> {
            None
        }

        fn open(&self, _info: &Self::FileInfo) -> Result<Self::Fd, ()> {
            Err(())
        }

        fn read(&self, _fd: Self::Fd, _buf: &mut [u8]) -> Result<usize, ()> {
            Err(())
        }

        fn write(&self, _fd: Self::Fd, _buf: &[u8]) -> Result<usize, ()> {
            Err(())
        }

        fn close(&self, _fd: Self::Fd) -> Result<(), ()> {
            Ok(())
        }
    }

    impl MemoryAccess for Arc<TestMemoryAccess> {
        type Mapping = TestMapping;

        fn create_mapping(
            &self,
            location: Location,
            size: usize,
            _allocation_strategy: AllocationStrategy,
        ) -> Result<Self::Mapping, CreateMappingError> {
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
            Ok(TestMapping { addr: ptr, size })
        }
    }

    impl MemoryRegionAccess for Arc<TestMemoryAccess> {
        type Region = TestRegion;

        fn create_and_track_mapping(
            &self,
            location: Location,
            size: usize,
            allocation_strategy: AllocationStrategy,
        ) -> Result<UserspacePtr<u8>, CreateMappingError> {
            let mapping = self.create_mapping(location, size, allocation_strategy)?;
            let addr = mapping.addr();
            
            self.mappings.lock().push((addr.addr(), mapping.size()));
            
            let region = TestRegion {
                addr: mapping.addr(),
                size: mapping.size(),
            };
            self.add_memory_region(region);
            Ok(addr)
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
            PROT_READ | PROT_WRITE,
            MAP_ANONYMOUS | MAP_PRIVATE,
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
            PROT_READ | PROT_WRITE,
            MAP_ANONYMOUS | MAP_PRIVATE,
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
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE, // Missing MAP_ANONYMOUS
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
            PROT_READ | PROT_WRITE,
            MAP_ANONYMOUS, // Missing MAP_PRIVATE
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
            PROT_READ | PROT_WRITE,
            MAP_ANONYMOUS | MAP_PRIVATE | MAP_FIXED,
            0,
            0,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), fixed_addr);
    }

    #[test]
    fn test_mmap_upper_half_rejected() {
        let cx = Arc::new(TestMemoryAccess::new());
        // Try to map to upper half (kernel space)
        let result = unsafe { UserspacePtr::<u8>::try_from_usize(0x8000_0000_0000_0000) };
        
        // Should fail to create the pointer itself
        assert!(result.is_err());
    }
}
