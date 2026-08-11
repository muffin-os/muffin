use core::alloc::Layout;
use core::ffi::c_void;
use core::fmt::{Debug, Formatter};

use kernel_memapi::{Guarded, Location, MemoryApi};
use kernel_virtual_memory::Segment;
use thiserror::Error;
use x86_64::VirtAddr;
use x86_64::registers::rflags::RFlags;
use x86_64::structures::paging::{PageSize, Size4KiB};

use crate::mem::memapi::{FrameContiguity, HigherHalfAllocation, HigherHalfMemoryApi, Writable};
use crate::{U64Ext, UsizeExt};

#[derive(Debug, Copy, Clone, Error)]
pub enum StackAllocationError {
    #[error("invalid stack page count")]
    InvalidPageCount,
    #[error("out of memory")]
    OutOfMemory,
}

pub struct HigherHalfStack {
    alloc: HigherHalfAllocation<Writable>,
    rsp: VirtAddr,
}

impl Debug for HigherHalfStack {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Stack")
            .field("alloc", &self.alloc)
            .field("rsp", &self.rsp)
            .finish_non_exhaustive()
    }
}

impl HigherHalfStack {
    /// Allocates a new stack with the given number of pages.
    ///
    /// # Errors
    /// Returns an error if stack memory couldn't be allocated, either
    /// physical or virtual.
    pub fn allocate(
        pages: usize,
        entry_point: extern "C" fn(*mut c_void),
        arg: *mut c_void,
        exit_fn: extern "C" fn(),
    ) -> Result<Self, StackAllocationError> {
        let mut stack = Self::allocate_plain(pages)?;

        // set up stack
        let entry_point = (entry_point as *const ()).cast::<usize>();
        let slice = stack.alloc.as_mut();
        slice.fill(0xCD);

        let mut writer = StackWriter::new(slice);
        writer.push(0xDEAD_BEEF_0BAD_F00D_DEAD_BEEF_0BAD_F00D_u128); // marker at stack bottom
        debug_assert_eq!(size_of_val(&exit_fn), size_of::<u64>());
        writer.push(exit_fn);
        let rsp = writer.offset - size_of::<Registers>();
        writer.push(Registers {
            rsp,
            rbp: 0,
            rdi: arg as usize,
            rip: entry_point as usize,
            rflags: (RFlags::IOPL_LOW | RFlags::INTERRUPT_FLAG)
                .bits()
                .into_usize(),
            ..Default::default()
        });

        stack.rsp = stack.alloc.start() + rsp.into_u64();
        Ok(stack)
    }

    /// Allocates a plain, unmodified stack with the given number of 4KiB pages.
    ///
    /// One page of the given count is reserved for the guard page below the stack,
    /// so that for `pages` pages, the usable stack size is `pages - 1`. The allocation
    /// is guarded, which additionally reserves an unmapped page above the stack.
    ///
    /// # Errors
    /// Returns an error if stack memory couldn't be allocated, either
    /// physical or virtual, or if mapping failed.
    pub fn allocate_plain(pages: usize) -> Result<Self, StackAllocationError> {
        let usable_bytes = pages
            .checked_sub(1)
            .and_then(|usable_pages| usable_pages.checked_mul(Size4KiB::SIZE.into_usize()))
            .ok_or(StackAllocationError::InvalidPageCount)?;
        let layout = Layout::from_size_align(usable_bytes, Size4KiB::SIZE.into_usize())
            .map_err(|_| StackAllocationError::InvalidPageCount)?;

        let alloc = HigherHalfMemoryApi
            .allocate(
                Location::Anywhere,
                layout,
                FrameContiguity::NonContiguous,
                Guarded::Yes,
            )
            .ok_or(StackAllocationError::OutOfMemory)?;

        let rsp = alloc.start() + alloc.len().into_u64();
        Ok(Self { alloc, rsp })
    }
}

impl HigherHalfStack {
    #[must_use]
    pub fn initial_rsp(&self) -> VirtAddr {
        self.rsp
    }

    /// The address one past the highest mapped byte of the stack, i.e. the
    /// value to load into RSP on a fresh entry. Use this for `TSS.RSP0`.
    #[must_use]
    pub fn top(&self) -> VirtAddr {
        self.alloc.start() + self.alloc.len().into_u64()
    }

    /// Returns the segment of the guard page directly below the usable stack.
    #[must_use]
    pub fn guard_page(&self) -> Segment {
        Segment::new(self.alloc.start() - Size4KiB::SIZE, Size4KiB::SIZE)
    }

    /// Returns the mapped segment, which is the part of the stack that is actually mapped in memory.
    #[must_use]
    pub fn mapped_segment(&self) -> Segment {
        Segment::new(self.alloc.start(), self.alloc.len().into_u64())
    }
}

#[repr(C, packed)]
#[derive(Debug, Default)]
struct Registers {
    r15: usize,
    r14: usize,
    r13: usize,
    r12: usize,
    r11: usize,
    r10: usize,
    r9: usize,
    r8: usize,
    rdi: usize,
    rsi: usize,
    rbp: usize,
    rsp: usize,
    rdx: usize,
    rcx: usize,
    rbx: usize,
    rax: usize,
    rflags: usize,
    rip: usize,
}

struct StackWriter<'a> {
    stack: &'a mut [u8],
    offset: usize,
}

impl<'a> StackWriter<'a> {
    fn new(stack: &'a mut [u8]) -> Self {
        let len = stack.len();
        Self { stack, offset: len }
    }

    fn push<T>(&mut self, value: T) {
        self.offset = self
            .offset
            .checked_sub(size_of::<T>())
            .expect("should not underflow stack during setup");
        let ptr = self
            .stack
            .as_mut_ptr()
            .wrapping_offset(
                isize::try_from(self.offset).expect("stack offset should not overflow isize"),
            )
            .cast::<T>();
        unsafe { ptr.write(value) };
    }
}
