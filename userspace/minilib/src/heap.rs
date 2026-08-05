//! Process heap, mapped on demand and grown in place.
//!
//! There is no setup call. A program that never allocates never maps a page.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};

use linked_list_allocator::{Heap, LockedHeap};

use crate::{MapFlags, ProtFlags, mmap};

const PAGE_SIZE: usize = 4096;

/// Base the heap grows upward from.
///
/// `MAP_FIXED` growth needs the range above the heap top to stay free. The kernel
/// places an addressless mapping by first fit from 4 GiB and commits a frame per
/// mapped byte, so nothing else reaches a terabyte up.
const HEAP_BASE: usize = 0x100_0000_0000;

/// Bytes per `mmap` while growing.
///
/// Frames for one mapping are physically contiguous, so a large request fails on
/// fragmentation where several small ones succeed.
const GROWTH_CHUNK: usize = 256 * 1024;

#[global_allocator]
static ALLOCATOR: GrowingHeap = GrowingHeap(LockedHeap::empty());

struct GrowingHeap(LockedHeap);

unsafe impl GlobalAlloc for GrowingHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut heap = self.0.lock();
        if let Ok(ptr) = heap.allocate_first_fit(layout) {
            return ptr.as_ptr();
        }
        // Alignment padding comes out of the same hole as the value, so the retry
        // cannot fail on slack.
        grow(&mut heap, layout.size() + layout.align());
        heap.allocate_first_fit(layout)
            .map_or(ptr::null_mut(), NonNull::as_ptr)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(ptr) = NonNull::new(ptr) else {
            return;
        };
        // SAFETY: `GlobalAlloc` guarantees `ptr` came from `alloc` under `layout`.
        unsafe { self.0.lock().deallocate(ptr, layout) }
    }
}

/// Maps at least `need` further bytes onto the top of the heap.
///
/// A failure part way keeps the bytes already mapped. Callers retry the allocation,
/// so there is no error to return.
fn grow(heap: &mut Heap, need: usize) {
    let target = need.max(GROWTH_CHUNK).next_multiple_of(PAGE_SIZE);
    let mut added = 0;
    while added < target {
        let chunk = (target - added).min(GROWTH_CHUNK);
        // The kernel asserts on an unaligned `MAP_FIXED` address. `top` stays page
        // aligned because the base is and every chunk is a page multiple.
        let at = if heap.bottom().is_null() {
            HEAP_BASE
        } else {
            heap.top() as usize
        };
        let mapped = mmap(
            at,
            chunk,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::ANONYMOUS | MapFlags::PRIVATE | MapFlags::FIXED,
            0,
            0,
        );
        // `MAP_FIXED` returns the requested address, so anything else is an errno.
        if mapped != at as isize {
            return;
        }
        // SAFETY: there is no munmap syscall, so the mapping outlives every use,
        // satisfying the heap's `'static` requirement. `extend` also needs the new
        // bytes directly above the old top, which mapping at `at` gives.
        unsafe {
            if heap.bottom().is_null() {
                heap.init(at as *mut u8, chunk);
            } else {
                heap.extend(chunk);
            }
        }
        added += chunk;
    }
}
