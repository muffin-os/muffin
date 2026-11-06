use kernel_abi::{EINVAL, ENOMEM, Errno, MapFlags, ProtFlags};
use tracing::{Level, instrument};
use x86_64::structures::paging::{PageSize, Size4KiB};

use crate::UserspacePtr;
use crate::access::{AllocationStrategy, Location, MemoryRegionAccess};

const PAGE_SIZE: usize = Size4KiB::SIZE as usize;

#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_mmap<Cx: MemoryRegionAccess>(
    cx: &Cx,
    addr: UserspacePtr<u8>,
    len: usize,
    prot: ProtFlags,
    flags: MapFlags,
    fd: Cx::Fd,
    offset: usize,
) -> Result<usize, Errno> {
    if len == 0 {
        return Err(EINVAL);
    }

    // W^X
    if prot.contains(ProtFlags::WRITE) && prot.contains(ProtFlags::EXEC) {
        return Err(EINVAL);
    }

    if flags.contains(MapFlags::ANONYMOUS) {
        if !flags.contains(MapFlags::PRIVATE) {
            return Err(EINVAL);
        }

        let location = if flags.contains(MapFlags::FIXED) {
            if addr.addr() == 0 || !addr.addr().is_multiple_of(PAGE_SIZE) {
                return Err(EINVAL);
            }
            addr.validate_range(len)?;
            Location::Fixed(addr)
        } else {
            Location::Anywhere
        };

        let mapped_addr = cx
            .create_and_track_mapping(location, len, AllocationStrategy::Eager, prot)
            .map_err(|e| match e {
                crate::access::CreateMappingError::LocationAlreadyMapped => EINVAL,
                crate::access::CreateMappingError::OutOfMemory => ENOMEM,
            })?;

        Ok(mapped_addr.addr())
    } else {
        // The kernel picks the address for a file-backed mapping, so a
        // caller-supplied one cannot be honored.
        if flags.contains(MapFlags::FIXED) {
            return Err(EINVAL);
        }

        let mapped_addr = match (
            flags.contains(MapFlags::SHARED),
            flags.contains(MapFlags::PRIVATE),
        ) {
            // A device mapping hands out the device's own frames, which sit at a
            // fixed physical base, so it carries no file offset.
            (true, false) => {
                if offset != 0 {
                    return Err(EINVAL);
                }
                cx.map_shared_file(fd, len, prot)?
            }
            (false, true) => cx.map_private_file(fd, offset, len, prot)?,
            _ => return Err(EINVAL),
        };
        Ok(mapped_addr.addr())
    }
}

/// This function removes the pages of `[addr, addr + len)` from the mappings
/// that overlap the range. `len` rounds up to a whole page. A page without a
/// mapping is not an error.
///
/// # Errors
/// This function returns `EINVAL` for a zero `len` and for an `addr` that is
/// not page aligned. It passes through each error of [`MemoryRegionAccess::unmap`].
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_munmap<Cx: MemoryRegionAccess>(
    cx: &Cx,
    addr: UserspacePtr<u8>,
    len: usize,
) -> Result<usize, Errno> {
    if len == 0 || !addr.addr().is_multiple_of(PAGE_SIZE) {
        return Err(EINVAL);
    }
    let len = len.checked_next_multiple_of(PAGE_SIZE).ok_or(EINVAL)?;
    cx.unmap(addr, len).map(|()| 0)
}

/// This function gives the pages of `[addr, addr + len)` the access `prot`.
/// `len` rounds up to a whole page. With an empty `prot`, each access to the
/// pages kills the task with `SIGSEGV`.
///
/// # Errors
/// This function returns `EINVAL` for a zero `len`, for an `addr` that is not
/// page aligned, and for a `prot` with `WRITE` and `EXEC` together. It passes
/// through each error of [`MemoryRegionAccess::protect`].
#[instrument(level = Level::TRACE, skip(cx))]
pub fn sys_mprotect<Cx: MemoryRegionAccess>(
    cx: &Cx,
    addr: UserspacePtr<u8>,
    len: usize,
    prot: ProtFlags,
) -> Result<usize, Errno> {
    if len == 0 || !addr.addr().is_multiple_of(PAGE_SIZE) {
        return Err(EINVAL);
    }
    if prot.contains(ProtFlags::WRITE | ProtFlags::EXEC) {
        return Err(EINVAL);
    }
    let len = len.checked_next_multiple_of(PAGE_SIZE).ok_or(EINVAL)?;
    cx.protect(addr, len, prot).map(|()| 0)
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use kernel_abi::{EINVAL, ENODEV, ENOMEM, Errno, MapFlags, ProtFlags};
    use spin::mutex::Mutex;

    use crate::UserspacePtr;
    use crate::access::{
        AllocationStrategy, CreateMappingError, Location, MemoryRegion, MemoryRegionAccess,
    };
    use crate::mman::{sys_mmap, sys_mprotect, sys_munmap};

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
        mappings: Mutex<Vec<(usize, usize, ProtFlags)>>, // (addr, size, prot)
        regions: Mutex<Vec<(usize, usize)>>,             // (addr, size)
        next_addr: Mutex<usize>,
        // Each tuple holds the address and the length of one unmap call.
        unmapped: Mutex<Vec<(usize, usize)>>,
        // Each tuple holds the address, the length and the protection of one protect call.
        protected: Mutex<Vec<(usize, usize, ProtFlags)>>,
        protect_result: Result<(), Errno>,
    }

    impl TestMemoryAccess {
        fn new() -> Self {
            Self {
                mappings: Mutex::new(Vec::new()),
                regions: Mutex::new(Vec::new()),
                next_addr: Mutex::new(0x1000), // Start at page boundary
                unmapped: Mutex::new(Vec::new()),
                protected: Mutex::new(Vec::new()),
                protect_result: Ok(()),
            }
        }
    }

    impl MemoryRegionAccess for Arc<TestMemoryAccess> {
        type Region = TestRegion;
        type Fd = i32;

        fn create_and_track_mapping(
            &self,
            location: Location,
            size: usize,
            _allocation_strategy: AllocationStrategy,
            prot: ProtFlags,
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
                    for (existing_addr, existing_size, _) in mappings.iter() {
                        if addr < existing_addr + existing_size && existing_addr < &(addr + size) {
                            return Err(CreateMappingError::LocationAlreadyMapped);
                        }
                    }
                    addr
                }
            };

            let ptr = UserspacePtr::try_from_usize(addr).unwrap();

            self.mappings.lock().push((addr, size, prot));

            let region = TestRegion { addr: ptr, size };
            self.add_memory_region(region);
            Ok(ptr)
        }

        fn add_memory_region(&self, region: Self::Region) {
            self.regions
                .lock()
                .push((region.addr().addr(), region.size()));
        }

        fn unmap(&self, addr: UserspacePtr<u8>, len: usize) -> Result<(), Errno> {
            self.unmapped.lock().push((addr.addr(), len));
            Ok(())
        }

        fn protect(
            &self,
            addr: UserspacePtr<u8>,
            len: usize,
            prot: ProtFlags,
        ) -> Result<(), Errno> {
            self.protected.lock().push((addr.addr(), len, prot));
            self.protect_result
        }
    }

    #[test]
    fn test_mmap_anonymous_private() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::ANONYMOUS | MapFlags::PRIVATE,
            0,
            0,
        );

        assert!(result.is_ok());
        let mapped_addr = result.unwrap();
        assert!(mapped_addr != 0);
        assert!(mapped_addr < (1_usize << 63)); // Lower half
    }

    #[test]
    fn anonymous_mapping_receives_prot() {
        for prot in [
            ProtFlags::READ,
            ProtFlags::READ | ProtFlags::WRITE,
            ProtFlags::READ | ProtFlags::EXEC,
        ] {
            let cx = Arc::new(TestMemoryAccess::new());
            let addr = UserspacePtr::try_from_usize(0).unwrap();

            let mapped = sys_mmap(
                &cx,
                addr,
                4096,
                prot,
                MapFlags::ANONYMOUS | MapFlags::PRIVATE,
                0,
                0,
            )
            .unwrap();

            assert_eq!(*cx.mappings.lock(), [(mapped, 4096, prot)]);
        }
    }

    #[test]
    fn anonymous_mapping_is_tracked_as_region() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let mapped = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ,
            MapFlags::ANONYMOUS | MapFlags::PRIVATE,
            0,
            0,
        )
        .unwrap();

        assert_eq!(*cx.regions.lock(), [(mapped, 4096)]);
    }

    #[test]
    fn test_mmap_zero_size() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let result = sys_mmap(
            &cx,
            addr,
            0,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::ANONYMOUS | MapFlags::PRIVATE,
            0,
            0,
        );

        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn test_mmap_not_private() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::ANONYMOUS, // Missing MAP_PRIVATE
            0,
            0,
        );

        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn test_mmap_fixed() {
        let cx = Arc::new(TestMemoryAccess::new());
        let fixed_addr = 0x100000;
        let addr = UserspacePtr::try_from_usize(fixed_addr).unwrap();

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::ANONYMOUS | MapFlags::PRIVATE | MapFlags::FIXED,
            0,
            0,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), fixed_addr);
    }

    #[test]
    fn mmap_fixed_unaligned_addr_rejected() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0x1001).unwrap();

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::ANONYMOUS | MapFlags::PRIVATE | MapFlags::FIXED,
            0,
            0,
        );

        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn test_mmap_write_exec_mutually_exclusive() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::WRITE | ProtFlags::EXEC, // Both WRITE and EXEC
            MapFlags::ANONYMOUS | MapFlags::PRIVATE,
            0,
            0,
        );

        assert_eq!(result, Err(EINVAL));
    }

    #[test]
    fn mmap_file_requires_exactly_one_sharing_mode() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        for flags in [MapFlags::empty(), MapFlags::SHARED | MapFlags::PRIVATE] {
            let result = sys_mmap(&cx, addr, 4096, ProtFlags::READ, flags, 3, 0);

            assert_eq!(
                result,
                Err(EINVAL),
                "a file mapping needs exactly one of MAP_SHARED or MAP_PRIVATE, got {flags:?}"
            );
        }
    }

    #[test]
    fn mmap_private_fd_unsupported() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let result = sys_mmap(&cx, addr, 4096, ProtFlags::READ, MapFlags::PRIVATE, 3, 0);

        assert_eq!(
            result,
            Err(ENODEV),
            "private fd mapping must hit the default map_private_file rejecting with ENODEV"
        );
    }

    #[test]
    fn mmap_private_file_fixed_rejected() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ,
            MapFlags::PRIVATE | MapFlags::FIXED,
            3,
            0,
        );

        assert_eq!(
            result,
            Err(EINVAL),
            "MAP_FIXED is not supported for a private file mapping"
        );
    }

    #[test]
    fn mmap_shared_fd_unsupported() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let result = sys_mmap(&cx, addr, 4096, ProtFlags::READ, MapFlags::SHARED, 3, 0);

        assert_eq!(
            result,
            Err(ENODEV),
            "shared fd mapping must hit the default map_shared_file rejecting with ENODEV"
        );
    }

    #[test]
    fn mmap_shared_offset_rejected() {
        let cx = Arc::new(TestMemoryAccess::new());
        let addr = UserspacePtr::try_from_usize(0).unwrap();

        let result = sys_mmap(
            &cx,
            addr,
            4096,
            ProtFlags::READ,
            MapFlags::SHARED,
            3,
            4096, // non-zero offset is unsupported
        );

        assert_eq!(
            result,
            Err(EINVAL),
            "shared fd mapping with a non-zero offset must be rejected"
        );
    }

    fn ptr(addr: usize) -> UserspacePtr<u8> {
        UserspacePtr::try_from_usize(addr).unwrap()
    }

    #[test]
    fn munmap_rejects_a_zero_len_and_an_unaligned_addr() {
        let cx = Arc::new(TestMemoryAccess::new());

        assert_eq!(Err(EINVAL), sys_munmap(&cx, ptr(0x2000), 0));
        assert_eq!(Err(EINVAL), sys_munmap(&cx, ptr(0x2001), 4096));
        assert_eq!(Err(EINVAL), sys_munmap(&cx, ptr(0x2000), usize::MAX));
        assert!(
            cx.unmapped.lock().is_empty(),
            "no invalid range reaches unmap"
        );
    }

    #[test]
    fn munmap_forwards_the_page_rounded_range() {
        let cx = Arc::new(TestMemoryAccess::new());

        assert_eq!(Ok(0), sys_munmap(&cx, ptr(0x2000), 1));
        assert_eq!(Ok(0), sys_munmap(&cx, ptr(0x4000), 8192));
        assert_eq!(Ok(0), sys_munmap(&cx, ptr(0x8000), 8193));
        assert_eq!(
            *cx.unmapped.lock(),
            [(0x2000, 4096), (0x4000, 8192), (0x8000, 12288)]
        );
    }

    #[test]
    fn mprotect_rejects_invalid_arguments() {
        let cx = Arc::new(TestMemoryAccess::new());

        assert_eq!(
            Err(EINVAL),
            sys_mprotect(&cx, ptr(0x2000), 0, ProtFlags::READ),
            "zero len"
        );
        assert_eq!(
            Err(EINVAL),
            sys_mprotect(&cx, ptr(0x2001), 4096, ProtFlags::READ),
            "unaligned addr"
        );
        assert_eq!(
            Err(EINVAL),
            sys_mprotect(&cx, ptr(0x2000), 4096, ProtFlags::WRITE | ProtFlags::EXEC),
            "write and exec together"
        );
        assert_eq!(
            Err(EINVAL),
            sys_mprotect(&cx, ptr(0x2000), usize::MAX, ProtFlags::READ),
            "len without a page-rounded value"
        );
        assert!(
            cx.protected.lock().is_empty(),
            "no invalid call reaches protect"
        );
    }

    #[test]
    fn mprotect_forwards_the_page_rounded_range_and_prot() {
        let cx = Arc::new(TestMemoryAccess::new());

        assert_eq!(Ok(0), sys_mprotect(&cx, ptr(0x2000), 1, ProtFlags::empty()));
        assert_eq!(
            Ok(0),
            sys_mprotect(&cx, ptr(0x4000), 8192, ProtFlags::READ | ProtFlags::EXEC)
        );
        assert_eq!(
            *cx.protected.lock(),
            [
                (0x2000, 4096, ProtFlags::empty()),
                (0x4000, 8192, ProtFlags::READ | ProtFlags::EXEC)
            ]
        );
    }

    #[test]
    fn mprotect_passes_through_the_error_of_protect() {
        let mut access = TestMemoryAccess::new();
        access.protect_result = Err(ENOMEM);
        let cx = Arc::new(access);

        assert_eq!(
            Err(ENOMEM),
            sys_mprotect(&cx, ptr(0x2000), 4096, ProtFlags::READ)
        );
    }
}
