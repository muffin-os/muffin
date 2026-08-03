use alloc::sync::Arc;
use core::cmp::Ordering;
use core::fmt::Debug;
use core::mem::ManuallyDrop;
use core::ops::Deref;

use conquer_once::spin::OnceCell;
use kernel_virtual_memory::{AlreadyReserved, Segment, VirtualMemoryManager};
use limine::memory_map::EntryType;
use spin::RwLock;
use x86_64::VirtAddr;
use x86_64::structures::paging::{PageSize, Size4KiB};

use crate::UsizeExt;
use crate::limine::{HHDM_REQUEST, KERNEL_ADDRESS_REQUEST, MEMORY_MAP_REQUEST};
use crate::mem::address_space::{RECURSIVE_INDEX, sign_extend_vaddr};
use crate::mem::heap::Heap;

static VMM: OnceCell<RwLock<VirtualMemoryManager>> = OnceCell::uninit();

fn vmm() -> &'static RwLock<VirtualMemoryManager> {
    VMM.get().expect("virtual memory should be initialized")
}

#[allow(clippy::missing_panics_doc)]
pub fn init() {
    VMM.init_once(|| {
        RwLock::new(VirtualMemoryManager::new(
            VirtAddr::new(0xFFFF_8000_0000_0000),
            0x0000_8000_0000_0000,
        ))
    });

    // recursive mapping
    {
        let recursive_index = *RECURSIVE_INDEX
            .get()
            .expect("recursive index should be initialized");
        let vaddr = VirtAddr::new(sign_extend_vaddr((recursive_index as u64) << 39));
        let len = 512 * 1024 * 1024 * 1024; // 512 GiB
        let segment = Segment::new(vaddr, len);
        let _ = VirtualMemoryHigherHalf
            .mark_as_reserved(segment)
            .expect("recursive index should not be reserved yet")
            .leak();
    }

    // kernel code
    {
        let kernel_addr = KERNEL_ADDRESS_REQUEST
            .get_response()
            .unwrap()
            .virtual_base();
        assert_eq!(
            kernel_addr, 0xffff_ffff_8000_0000,
            "kernel address should be 0xffff_ffff_8000_0000, if it isn't, either check the linker file or you know what you're doing"
        );

        let kernel_code_segment = Segment::new(
            VirtAddr::new(kernel_addr),
            usize::MAX.into_u64() - kernel_addr + 1,
        );
        let _ = VirtualMemoryHigherHalf
            .mark_as_reserved(kernel_code_segment)
            .expect("kernel code segment should not be reserved yet")
            .leak();
    }

    // kernel file and bootloader reclaimable
    {
        let hhdm_offset = HHDM_REQUEST.get_response().unwrap().offset();
        MEMORY_MAP_REQUEST
            .get_response()
            .unwrap()
            .entries()
            .iter()
            .filter(|e| {
                [
                    EntryType::EXECUTABLE_AND_MODULES,
                    EntryType::BOOTLOADER_RECLAIMABLE,
                ]
                .contains(&e.entry_type)
            })
            .for_each(|e| {
                let segment = Segment::new(VirtAddr::new(e.base + hhdm_offset), e.length);
                let _ = VirtualMemoryHigherHalf
                    .mark_as_reserved(segment)
                    .expect("segment should not be reserved yet")
                    .leak();
            });
    }

    // heap
    let _ = VirtualMemoryHigherHalf
        .mark_as_reserved(Segment::new(Heap::bottom(), Heap::size() as u64))
        .expect("heap should not be reserved yet")
        .leak();
}

enum InnerVmm<'vmm> {
    Ref(&'vmm RwLock<VirtualMemoryManager>),
    Rc(Arc<RwLock<VirtualMemoryManager>>),
}

impl Deref for InnerVmm<'_> {
    type Target = RwLock<VirtualMemoryManager>;

    fn deref(&self) -> &Self::Target {
        match self {
            InnerVmm::Ref(vmm) => vmm,
            InnerVmm::Rc(vmm) => vmm,
        }
    }
}

#[must_use]
pub struct OwnedSegment<'vmm> {
    vmm: InnerVmm<'vmm>,
    inner: Segment,
}

impl OwnedSegment<'_> {
    pub fn new_ref(vmm: &'static RwLock<VirtualMemoryManager>, inner: Segment) -> Self {
        Self {
            vmm: InnerVmm::Ref(vmm),
            inner,
        }
    }

    pub fn new_rc(vmm: Arc<RwLock<VirtualMemoryManager>>, inner: Segment) -> Self {
        Self {
            vmm: InnerVmm::Rc(vmm),
            inner,
        }
    }
}

impl PartialEq<Self> for OwnedSegment<'_> {
    fn eq(&self, other: &Self) -> bool {
        let my_vmm = self.vmm.read();
        let other_vmm = other.vmm.read();
        *my_vmm == *other_vmm
    }
}

impl Eq for OwnedSegment<'_> {}

impl PartialOrd<Self> for OwnedSegment<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OwnedSegment<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.inner.cmp(&other.inner)
    }
}

impl Debug for OwnedSegment<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OwnedSegment")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl OwnedSegment<'_> {
    #[must_use]
    pub fn leak(self) -> Segment {
        ManuallyDrop::new(self).inner
    }
}

impl Drop for OwnedSegment<'_> {
    fn drop(&mut self) {
        self.vmm.write().release(self.inner);
    }
}

impl Deref for OwnedSegment<'_> {
    type Target = Segment;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub trait VirtualMemoryAllocator {
    /// Returns a segment of virtual memory that is reserved for the kernel.
    /// The size is exactly `pages * 4096` bytes.
    /// The start address of the returned segment is aligned to `4096` bytes.
    fn reserve(&self, pages: usize) -> Option<OwnedSegment<'static>>;

    /// # Errors
    /// This function returns an error if the segment is already reserved.
    fn mark_as_reserved(&self, segment: Segment) -> Result<OwnedSegment<'static>, AlreadyReserved>;

    /// # Safety
    /// The caller must ensure that the segment is not used after releasing it,
    /// and that the segment was previously reserved by this virtual memory manager.
    unsafe fn release(&self, segment: Segment) -> bool;
}

pub struct VirtualMemoryHigherHalf;

impl VirtualMemoryAllocator for VirtualMemoryHigherHalf {
    #[allow(clippy::missing_panics_doc)] // panic must not happen, so the caller shouldn't have to care about it
    fn reserve(&self, pages: usize) -> Option<OwnedSegment<'static>> {
        vmm()
            .write()
            .reserve(pages * 4096)
            .map(|segment| OwnedSegment::new_ref(vmm(), segment))
            .inspect(|segment| assert!(segment.start.is_aligned(Size4KiB::SIZE)))
    }

    fn mark_as_reserved(&self, segment: Segment) -> Result<OwnedSegment<'static>, AlreadyReserved> {
        assert!(segment.start.is_aligned(Size4KiB::SIZE));
        assert_eq!(segment.len % Size4KiB::SIZE, 0);

        vmm()
            .write()
            .mark_as_reserved(segment)
            .map(|()| OwnedSegment::new_ref(vmm(), segment))
    }

    unsafe fn release(&self, segment: Segment) -> bool {
        vmm().write().release(segment)
    }
}

impl VirtualMemoryAllocator for Arc<RwLock<VirtualMemoryManager>> {
    fn reserve(&self, pages: usize) -> Option<OwnedSegment<'static>> {
        self.write()
            .reserve(pages * 4096)
            .map(|segment| OwnedSegment::new_rc(self.clone(), segment))
    }

    fn mark_as_reserved(&self, segment: Segment) -> Result<OwnedSegment<'static>, AlreadyReserved> {
        assert!(segment.start.is_aligned(Size4KiB::SIZE));
        assert_eq!(segment.len % Size4KiB::SIZE, 0);

        self.write()
            .mark_as_reserved(segment)
            .map(|()| OwnedSegment::new_rc(self.clone(), segment))
    }

    unsafe fn release(&self, segment: Segment) -> bool {
        self.write().release(segment)
    }
}
