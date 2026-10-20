use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering::Relaxed;

use conquer_once::spin::OnceCell;
use tracing::info;
use x86_64::VirtAddr;
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::{Page, PageTableFlags, Size2MiB, Size4KiB};

use crate::mem::address_space::{AddressSpace, virt_addr_from_page_table_indices};
use crate::mem::phys::PhysicalMemory;

static HEAP_INITIALIZED: AtomicBool = AtomicBool::new(false);
static HEAP_START: VirtAddr = virt_addr_from_page_table_indices([257, 0, 0, 0], 0);

/// Runtime-initialized heap sizes based on available physical memory.
static HEAP_SIZES: OnceCell<HeapSizes> = OnceCell::uninit();

struct HeapSizes {
    /// Initial heap size for stage1
    initial: usize,
    /// Total heap size after stage2
    total: usize,
}

impl HeapSizes {
    /// Both sizes are multiples of 2 MiB, because stage 2 maps the heap
    /// extension with 2 MiB pages.
    fn from_physical_memory(usable_ram_bytes: usize) -> Self {
        const MIB: usize = 1024 * 1024;
        const MIB_2: usize = 2 * MIB;

        // About 0.1% of RAM, which is more than enough for Vec<FrameState>.
        let initial = {
            let clamped = (usable_ram_bytes / 1024).clamp(MIB_2, 128 * MIB);
            clamped.div_ceil(MIB_2) * MIB_2
        };

        // About 0.4% of RAM for all kernel heap needs.
        let total = {
            let clamped = (usable_ram_bytes / 256).clamp(initial + MIB_2, 512 * MIB);
            clamped.div_ceil(MIB_2) * MIB_2
        };

        Self { initial, total }
    }

    fn initial(&self) -> usize {
        self.initial
    }

    fn total(&self) -> usize {
        self.total
    }
}

static MAIN_HEAP: linked_list_allocator::LockedHeap = linked_list_allocator::LockedHeap::empty();

/// Size of the bootstrap heap used before the main heap is mapped.
const BOOTSTRAP_HEAP_SIZE: usize = 16 * 1024;

static BOOTSTRAP_HEAP: spin::Mutex<linked_list_allocator::Heap> =
    spin::Mutex::new(linked_list_allocator::Heap::empty());

/// Backing storage for the bootstrap heap.
///
/// The tracing global dispatch is registered before `mem::init`, and `Dispatch::new`
/// performs one small `Arc` allocation. That allocation has to succeed before the
/// main heap exists, so it is served from this static region.
struct BootstrapBuffer(UnsafeCell<[u8; BOOTSTRAP_HEAP_SIZE]>);

// Safety: access is serialized through the BOOTSTRAP_HEAP mutex.
unsafe impl Sync for BootstrapBuffer {}

static BOOTSTRAP_BUFFER: BootstrapBuffer =
    BootstrapBuffer(UnsafeCell::new([0; BOOTSTRAP_HEAP_SIZE]));

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

struct KernelAllocator;

impl KernelAllocator {
    fn bootstrap_range() -> (usize, usize) {
        let start = BOOTSTRAP_BUFFER.0.get().cast::<u8>() as usize;
        (start, start + BOOTSTRAP_HEAP_SIZE)
    }
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if HEAP_INITIALIZED.load(Relaxed) {
            return unsafe { MAIN_HEAP.alloc(layout) };
        }

        let mut heap = BOOTSTRAP_HEAP.lock();
        if heap.size() == 0 {
            // Safety: the buffer is a static region of exactly BOOTSTRAP_HEAP_SIZE bytes,
            // initialized once under the mutex.
            unsafe { heap.init(BOOTSTRAP_BUFFER.0.get().cast(), BOOTSTRAP_HEAP_SIZE) };
        }
        heap.allocate_first_fit(layout)
            .map_or(core::ptr::null_mut(), NonNull::as_ptr)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let (start, end) = Self::bootstrap_range();
        let addr = ptr as usize;
        if addr >= start && addr < end {
            if let Some(ptr) = NonNull::new(ptr) {
                unsafe { BOOTSTRAP_HEAP.lock().deallocate(ptr, layout) };
            }
        } else {
            unsafe { MAIN_HEAP.dealloc(ptr, layout) };
        }
    }
}

pub(in crate::mem) fn init(address_space: &AddressSpace, usable_physical_memory_bytes: usize) {
    assert!(PhysicalMemory::is_initialized());

    let heap_sizes = HeapSizes::from_physical_memory(usable_physical_memory_bytes);
    info!(
        "heap sizes: initial={} MiB, total={} MiB (for {} MiB RAM)",
        heap_sizes.initial() / 1024 / 1024,
        heap_sizes.total() / 1024 / 1024,
        usable_physical_memory_bytes / 1024 / 1024
    );
    HEAP_SIZES.init_once(|| heap_sizes);

    let initial_heap_size = HEAP_SIZES.get().unwrap().initial();

    info!("initializing heap at {HEAP_START:p}");
    let page_range = PageRangeInclusive::<Size4KiB> {
        start: Page::containing_address(HEAP_START),
        end: Page::containing_address(HEAP_START + initial_heap_size as u64 - 1),
    };

    address_space
        .map_range(
            page_range,
            PhysicalMemory::allocate_frames_non_contiguous(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        )
        .expect("should be able to map heap");

    unsafe {
        MAIN_HEAP
            .lock()
            .init(HEAP_START.as_mut_ptr(), initial_heap_size);
    }

    HEAP_INITIALIZED.store(true, Relaxed);
}

// In stage2, we already have the physical memory manager that uses the heap, which is much faster
// than the one we use on boot, so we allocate the largest portion of memory for the heap in stage2.
pub(in crate::mem) fn init_stage2() {
    assert!(HEAP_INITIALIZED.load(Relaxed));

    let heap_sizes = HEAP_SIZES.get().expect("heap sizes should be initialized");
    let initial_heap_size = heap_sizes.initial();
    let total_heap_size = heap_sizes.total();

    let new_start = HEAP_START + initial_heap_size as u64;

    let page_range = PageRangeInclusive::<Size2MiB> {
        start: Page::containing_address(new_start),
        end: Page::containing_address(new_start + (total_heap_size - initial_heap_size) as u64),
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
        MAIN_HEAP.lock().extend(total_heap_size - initial_heap_size);
    }
}

#[derive(Copy, Clone)]
pub struct Heap;

impl Heap {
    pub fn is_initialized() -> bool {
        HEAP_INITIALIZED.load(Relaxed)
    }

    pub fn free() -> usize {
        MAIN_HEAP.lock().free()
    }

    pub fn used() -> usize {
        MAIN_HEAP.lock().used()
    }

    pub fn size() -> usize {
        MAIN_HEAP.lock().size()
    }

    pub fn bottom() -> VirtAddr {
        VirtAddr::new(MAIN_HEAP.lock().bottom() as u64)
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
