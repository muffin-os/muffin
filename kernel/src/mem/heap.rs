use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering::Relaxed;

use log::info;
use x86_64::VirtAddr;
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::{Page, PageTableFlags, Size2MiB, Size4KiB};

use crate::mem::address_space::{AddressSpace, virt_addr_from_page_table_indices};
use crate::mem::phys::PhysicalMemory;

static HEAP_INITIALIZED: AtomicBool = AtomicBool::new(false);
static HEAP_START: VirtAddr = virt_addr_from_page_table_indices([257, 0, 0, 0], 0);

/// Since stage1 (which is when we initialize the heap) is slow in allocating physical memory,
/// we allocate a small portion of memory for the heap in stage1 and then allocate the rest in stage2.
static INITIAL_HEAP_SIZE: usize = 2 * 1024 * 1024; // 2MiB

/// The amount of heap that is available after the stage2 initialization.
pub static HEAP_SIZE: usize = 32 * 1024 * 1024; // 32MiB

#[global_allocator]
static ALLOCATOR: linked_list_allocator::LockedHeap = linked_list_allocator::LockedHeap::empty();

pub(in crate::mem) fn init(address_space: &AddressSpace) {
    assert!(PhysicalMemory::is_initialized());

    info!("initializing heap at {HEAP_START:p}");
    let page_range = PageRangeInclusive::<Size4KiB> {
        start: Page::containing_address(HEAP_START),
        end: Page::containing_address(HEAP_START + INITIAL_HEAP_SIZE as u64 - 1),
    };

    address_space
        .map_range(
            page_range,
            PhysicalMemory::allocate_frames_non_contiguous(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        )
        .expect("should be able to map heap");

    unsafe {
        ALLOCATOR
            .lock()
            .init(HEAP_START.as_mut_ptr(), INITIAL_HEAP_SIZE);
    }

    HEAP_INITIALIZED.store(true, Relaxed);
}

// In stage2, we already have the physical memory manager that uses the heap, which is much faster
// than the one we use on boot, so we allocate the largest portion of memory for the heap in stage2.
pub(in crate::mem) fn init_stage2() {
    assert!(HEAP_INITIALIZED.load(Relaxed));

    let new_start = HEAP_START + INITIAL_HEAP_SIZE as u64;

    let page_range = PageRangeInclusive::<Size2MiB> {
        start: Page::containing_address(new_start),
        end: Page::containing_address(new_start + (HEAP_SIZE - INITIAL_HEAP_SIZE) as u64),
    };

    let address_space = AddressSpace::kernel();
    address_space
        .map_range(
            page_range,
            PhysicalMemory::allocate_frames_non_contiguous(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        )
        .expect("should be able to map more heap");

    unsafe {
        ALLOCATOR.lock().extend(HEAP_SIZE - INITIAL_HEAP_SIZE);
    }
}

#[derive(Copy, Clone)]
pub struct Heap;

impl Heap {
    pub fn is_initialized() -> bool {
        HEAP_INITIALIZED.load(Relaxed)
    }

    pub fn free() -> usize {
        ALLOCATOR.lock().free()
    }

    pub fn used() -> usize {
        ALLOCATOR.lock().used()
    }

    pub fn size() -> usize {
        ALLOCATOR.lock().size()
    }

    pub fn bottom() -> VirtAddr {
        VirtAddr::new(ALLOCATOR.lock().bottom() as u64)
    }
}

impl core::fmt::Debug for Heap {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("Heap")
            .field("initialized", &Self::is_initialized())
            .field("free", &Self::free())
            .field("used", &Self::used())
            .field("size", &Self::size())
            .finish()
    }
}
