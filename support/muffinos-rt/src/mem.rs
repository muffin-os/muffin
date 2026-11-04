//! `malloc` and friends over the `muffin_sys` heap.
//!
//! `GlobalAlloc::dealloc` needs the layout of the block, but `free` gets only
//! the pointer. Each block therefore starts with `align` bytes of padding that
//! end in the block's `Layout`, and the caller's bytes follow the padding.

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::{c_int, c_void};
use core::ptr;

use kernel_abi::{EINVAL, ENOMEM};
use muffin_sys::GrowingHeap;

use crate::errno::fail;

static HEAP: GrowingHeap = GrowingHeap::new();

/// Alignment `malloc` guarantees, the alignment of `max_align_t` on x86_64.
const MIN_ALIGN: usize = 16;

const _: () = assert!(size_of::<Layout>() <= MIN_ALIGN);

/// Returns the block layout for `size` caller bytes at `align`, a power of
/// two, or `None` when the block size overflows.
fn block_layout(size: usize, align: usize) -> Option<Layout> {
    debug_assert!(align.is_power_of_two());
    let padding = align.max(MIN_ALIGN);
    Layout::from_size_align(size.checked_add(padding)?, padding).ok()
}

/// Returns the caller's bytes of a new block, or null when `heap` is out of
/// memory.
///
/// # Safety
///
/// `layout` must come from [`block_layout`].
unsafe fn allocate(heap: &impl GlobalAlloc, layout: Layout) -> *mut u8 {
    // SAFETY: `block_layout` never returns a zero size.
    let block = unsafe { heap.alloc(layout) };
    if block.is_null() {
        return block;
    }
    // SAFETY: the padding is `layout.align()` bytes long and at least
    // `MIN_ALIGN` bytes, so the `Layout` fits in its aligned end.
    unsafe {
        let data = block.add(layout.align());
        data.cast::<Layout>().sub(1).write(layout);
        data
    }
}

/// # Safety
///
/// `data` must come from [`allocate`] or [`reallocate`] and be live.
unsafe fn layout_of(data: *mut u8) -> Layout {
    unsafe { data.cast::<Layout>().sub(1).read() }
}

/// # Safety
///
/// `data` must come from [`allocate`] or [`reallocate`] on `heap` and be live.
unsafe fn release(heap: &impl GlobalAlloc, data: *mut u8) {
    unsafe {
        let layout = layout_of(data);
        heap.dealloc(data.sub(layout.align()), layout);
    }
}

/// Moves `data` to a block with `size` caller bytes and returns the new caller
/// bytes, or null when `heap` is out of memory. On null, `data` stays live.
///
/// # Safety
///
/// `data` must come from [`allocate`] or [`reallocate`] on `heap` and be live.
unsafe fn reallocate(heap: &impl GlobalAlloc, data: *mut u8, size: usize) -> *mut u8 {
    let old = unsafe { layout_of(data) };
    let Some(new) = block_layout(size, old.align()) else {
        return ptr::null_mut();
    };
    // SAFETY: the block starts `old.align()` bytes before `data`, and `new`
    // keeps the alignment of `old`.
    unsafe {
        let block = heap.realloc(data.sub(old.align()), old, new.size());
        if block.is_null() {
            return block;
        }
        let data = block.add(new.align());
        data.cast::<Layout>().sub(1).write(new);
        data
    }
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn malloc(size: usize) -> *mut c_void {
    let Some(layout) = block_layout(size, MIN_ALIGN) else {
        return fail(ENOMEM);
    };
    let data = unsafe { allocate(&HEAP, layout) };
    if data.is_null() {
        return fail(ENOMEM);
    }
    data.cast()
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    let Some(total) = count.checked_mul(size) else {
        return fail(ENOMEM);
    };
    let data = malloc(total);
    if !data.is_null() {
        // SAFETY: `malloc` returned `total` writable bytes.
        unsafe { data.write_bytes(0, total) };
    }
    data
}

/// # Safety
///
/// `data` must be null or a live pointer from this crate's allocator.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn realloc(data: *mut c_void, size: usize) -> *mut c_void {
    if data.is_null() {
        return malloc(size);
    }
    let moved = unsafe { reallocate(&HEAP, data.cast(), size) };
    if moved.is_null() {
        return fail(ENOMEM);
    }
    moved.cast()
}

/// # Safety
///
/// `data` must be null or a live pointer from this crate's allocator.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn free(data: *mut c_void) {
    if !data.is_null() {
        unsafe { release(&HEAP, data.cast()) };
    }
}

/// # Safety
///
/// `out` must be valid for one pointer write.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
    if !align.is_power_of_two() || align < size_of::<*mut c_void>() {
        return EINVAL.into();
    }
    let Some(layout) = block_layout(size, align) else {
        return ENOMEM.into();
    };
    let data = unsafe { allocate(&HEAP, layout) };
    if data.is_null() {
        return ENOMEM.into();
    }
    unsafe { out.write(data.cast()) };
    0
}

#[cfg(test)]
mod tests {
    use std::alloc::System;

    use super::*;

    #[test]
    fn block_keeps_caller_alignment() {
        for align in [1, 8, 16, 64, 4096] {
            let layout = block_layout(24, align).unwrap();
            let data = unsafe { allocate(&System, layout) };
            assert_eq!(
                data as usize % align.max(MIN_ALIGN),
                0,
                "caller bytes of an align {align} block are aligned"
            );
            unsafe { release(&System, data) };
        }
    }

    #[test]
    fn block_layout_rejects_overflowing_sizes() {
        assert_eq!(
            block_layout(usize::MAX, 16),
            None,
            "size plus padding overflows"
        );
        assert_eq!(
            block_layout(isize::MAX as usize - 8, 16),
            None,
            "a block larger than isize::MAX is no layout"
        );
    }

    #[test]
    fn reallocate_keeps_bytes_and_alignment() {
        let data = unsafe { allocate(&System, block_layout(4, 64).unwrap()) };
        unsafe { data.copy_from(b"abcd".as_ptr(), 4) };

        let grown = unsafe { reallocate(&System, data, 4096) };

        assert_eq!(grown as usize % 64, 0, "the new block keeps align 64");
        let bytes = unsafe { core::slice::from_raw_parts(grown, 4) };
        assert_eq!(bytes, b"abcd", "the old bytes move to the new block");
        assert_eq!(
            unsafe { layout_of(grown) },
            block_layout(4096, 64).unwrap(),
            "the header records the new size"
        );
        unsafe { release(&System, grown) };
    }
}
