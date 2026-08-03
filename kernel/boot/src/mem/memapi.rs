use alloc::sync::Arc;
use core::alloc::Layout;
use core::fmt::{Debug, Formatter};
use core::marker::PhantomData;
use core::ops::Deref;
use core::slice::{from_raw_parts, from_raw_parts_mut};

use kernel_memapi::{Allocation, Guarded, Location, MemoryApi, UserAccessible, WritableAllocation};
use kernel_virtual_memory::Segment;
use x86_64::VirtAddr;
use x86_64::structures::paging::{PageSize, PageTableFlags, Size4KiB};

use crate::mcore::mtask::process::Process;
use crate::mem::phys::PhysicalMemory;
use crate::mem::virt::{OwnedSegment, VirtualMemoryAllocator};
use crate::{U64Ext, UsizeExt};

#[derive(Clone)]
pub struct LowerHalfMemoryApi {
    process: Arc<Process>,
}

impl LowerHalfMemoryApi {
    pub fn new(process: Arc<Process>) -> Self {
        Self { process }
    }
}

impl MemoryApi for LowerHalfMemoryApi {
    type ReadonlyAllocation = LowerHalfAllocation<Readonly>;
    type WritableAllocation = LowerHalfAllocation<Writable>;
    type ExecutableAllocation = LowerHalfAllocation<Executable>;

    fn allocate(
        &mut self,
        location: Location,
        layout: Layout,
        user_accessible: UserAccessible,
        guarded: Guarded,
    ) -> Option<Self::WritableAllocation> {
        assert!(layout.align() <= Size4KiB::SIZE.into_usize());

        let num_pages = layout.size().div_ceil(Size4KiB::SIZE.into_usize())
            + match guarded {
                Guarded::Yes => 2, // Reserve two extra pages for guard pages
                Guarded::No => 0,
            };

        let (start, segment) = match location {
            Location::Anywhere => {
                let segment = self.process.vmm().reserve(num_pages)?;
                (None, segment)
            }
            Location::Fixed(v) => {
                let aligned_start_addr = v.align_down(Size4KiB::SIZE)
                    - match guarded {
                        Guarded::Yes => Size4KiB::SIZE,
                        Guarded::No => 0,
                    };
                let aligned_end_addr = (v + layout.size().into_u64()).align_up(Size4KiB::SIZE)
                    + match guarded {
                        Guarded::Yes => Size4KiB::SIZE,
                        Guarded::No => 0,
                    };
                let segment =
                    Segment::new(aligned_start_addr, aligned_end_addr - aligned_start_addr);
                let vmm = self.process.vmm();
                let segment = vmm.mark_as_reserved(segment).ok()?;
                (Some(v), segment)
            }
        };

        let mapped_segment = match guarded {
            Guarded::Yes => Segment::new(
                segment.start + Size4KiB::SIZE,
                segment.len - (2 * Size4KiB::SIZE),
            ),
            Guarded::No => *segment,
        };

        self.process
            .address_space()
            .map_range::<Size4KiB>(
                &mapped_segment,
                PhysicalMemory::allocate_frames_non_contiguous(),
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::NO_EXECUTE
                    | if user_accessible == UserAccessible::Yes {
                        PageTableFlags::USER_ACCESSIBLE
                    } else {
                        PageTableFlags::empty()
                    },
            )
            .ok()?;

        let start = start.unwrap_or(mapped_segment.start);
        Some(LowerHalfAllocation {
            start,
            layout,
            inner: Inner {
                process: self.process.clone(),
                segment,
                mapped_segment,
            },
            _typ: PhantomData,
        })
    }

    fn make_executable(
        &mut self,
        allocation: Self::WritableAllocation,
    ) -> Result<Self::ExecutableAllocation, Self::WritableAllocation> {
        let res = self.process.address_space().remap_range::<Size4KiB, _>(
            &*allocation.segment,
            |mut flags| {
                flags.remove(PageTableFlags::WRITABLE);
                flags.remove(PageTableFlags::NO_EXECUTE);
                flags
            },
        );
        if res.is_err() {
            return Err(allocation);
        }

        Ok(LowerHalfAllocation {
            start: allocation.start,
            layout: allocation.layout,
            inner: allocation.inner,
            _typ: PhantomData,
        })
    }

    fn make_writable(
        &mut self,
        allocation: Self::ExecutableAllocation,
    ) -> Result<Self::WritableAllocation, Self::ExecutableAllocation> {
        let res = self.process.address_space().remap_range::<Size4KiB, _>(
            &*allocation.segment,
            |mut flags| {
                flags.insert(PageTableFlags::WRITABLE);
                flags.insert(PageTableFlags::NO_EXECUTE);
                flags
            },
        );
        if res.is_err() {
            return Err(allocation);
        }

        Ok(LowerHalfAllocation {
            start: allocation.start,
            layout: allocation.layout,
            inner: allocation.inner,
            _typ: PhantomData,
        })
    }

    fn make_readonly(
        &mut self,
        allocation: Self::WritableAllocation,
    ) -> Result<Self::ReadonlyAllocation, Self::WritableAllocation> {
        let res = self.process.address_space().remap_range::<Size4KiB, _>(
            &*allocation.segment,
            |mut flags| {
                flags.remove(PageTableFlags::WRITABLE);
                flags.insert(PageTableFlags::NO_EXECUTE);
                flags
            },
        );
        if res.is_err() {
            return Err(allocation);
        }

        Ok(LowerHalfAllocation {
            start: allocation.start,
            layout: allocation.layout,
            inner: allocation.inner,
            _typ: PhantomData,
        })
    }
}

trait Sealed {}
#[allow(private_bounds)]
pub trait AllocationType: Sealed {}
#[derive(Debug)]
pub struct Readonly;
impl Sealed for Readonly {}
impl AllocationType for Readonly {}
#[derive(Debug)]
pub struct Writable;
impl Sealed for Writable {}
impl AllocationType for Writable {}
#[derive(Debug)]
pub struct Executable;
impl Sealed for Executable {}
impl AllocationType for Executable {}

pub struct LowerHalfAllocation<T> {
    start: VirtAddr,
    layout: Layout,
    inner: Inner,
    _typ: PhantomData<T>,
}

impl<T: AllocationType> LowerHalfAllocation<T> {
    #[must_use]
    pub fn start(&self) -> VirtAddr {
        self.start
    }

    #[allow(clippy::len_without_is_empty)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.layout.size()
    }
}

pub struct Inner {
    segment: OwnedSegment<'static>,
    mapped_segment: Segment,
    process: Arc<Process>,
}

impl<T: AllocationType> Deref for LowerHalfAllocation<T> {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: AllocationType> Debug for LowerHalfAllocation<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LowerHalfAllocation")
            .field("process_id", &self.process.pid())
            .field("segment", &self.segment)
            .field("typ", &self._typ)
            .finish_non_exhaustive()
    }
}

impl<T: AllocationType> AsRef<[u8]> for LowerHalfAllocation<T> {
    fn as_ref(&self) -> &[u8] {
        let ptr = self.start.as_ptr();
        unsafe { from_raw_parts(ptr, self.layout.size()) }
    }
}

impl<T: AllocationType> Allocation for LowerHalfAllocation<T> {
    fn layout(&self) -> Layout {
        self.layout
    }
}

impl AsMut<[u8]> for LowerHalfAllocation<Writable> {
    fn as_mut(&mut self) -> &mut [u8] {
        let ptr = self.start.as_mut_ptr();
        unsafe { from_raw_parts_mut(ptr, self.layout.size()) }
    }
}

impl WritableAllocation for LowerHalfAllocation<Writable> {}

impl Drop for Inner {
    fn drop(&mut self) {
        self.process
            .address_space()
            .unmap_range::<Size4KiB>(&self.mapped_segment, PhysicalMemory::deallocate_frame);
    }
}
