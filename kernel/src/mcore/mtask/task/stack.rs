use core::ffi::c_void;
use core::fmt::{Debug, Formatter};
use core::slice::from_raw_parts_mut;

use kernel_virtual_memory::Segment;
use thiserror::Error;
use x86_64::VirtAddr;
use x86_64::registers::rflags::RFlags;
use x86_64::structures::paging::{PageSize, PageTableFlags, Size4KiB};

use crate::mem::address_space::AddressSpace;
use crate::mem::phys::PhysicalMemory;
use crate::mem::virt::{OwnedSegment, VirtualMemoryAllocator, VirtualMemoryHigherHalf};
use crate::{U64Ext, UsizeExt};

#[derive(Debug, Copy, Clone, Error)]
pub enum StackAllocationError {
    #[error("out of virtual memory")]
    OutOfVirtualMemory,
    #[error("out of physical memory")]
    OutOfPhysicalMemory,
}

pub struct HigherHalfStack {
    segment: OwnedSegment<'static>,
    mapped_segment: Segment,
    rsp: VirtAddr,
}

impl Debug for HigherHalfStack {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Stack")
            .field("segment", &self.segment)
            .finish_non_exhaustive()
    }
}

impl Drop for HigherHalfStack {
    fn drop(&mut self) {
        let address_space = AddressSpace::kernel();
        address_space.unmap_range::<Size4KiB>(&*self.segment, PhysicalMemory::deallocate_frame);
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
        let mapped_segment = stack.mapped_segment;

        // set up stack
        let entry_point = (entry_point as *const ()).cast::<usize>();
        let slice = unsafe {
            from_raw_parts_mut(
                mapped_segment.start.as_mut_ptr::<u8>(),
                mapped_segment.len.into_usize(),
            )
        };
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

        stack.rsp = mapped_segment.start + rsp.into_u64();
        Ok(stack)
    }

    /// Allocates a plain, unmodified stack with the given number of 4KiB pages.
    /// The stack will be mapped according to the given arguments.
    ///
    /// One page is reserved for the guard page, which is not mapped. It is at the
    /// bottom of the stack. This implies that for `pages` pages, the usable stack
    /// size is `pages - 1`.
    ///
    /// # Errors
    /// Returns an error if stack memory couldn't be allocated, either
    /// physical or virtual, or if mapping failed.
    pub fn allocate_plain(pages: usize) -> Result<Self, StackAllocationError> {
        let segment = VirtualMemoryHigherHalf
            .reserve(pages)
            .ok_or(StackAllocationError::OutOfVirtualMemory)?;

        let mapped_segment =
            Segment::new(segment.start + Size4KiB::SIZE, segment.len - Size4KiB::SIZE);

        AddressSpace::kernel()
            .map_range::<Size4KiB>(
                &mapped_segment,
                PhysicalMemory::allocate_frames_non_contiguous(),
                // FIXME: must be user accessible for user tasks, but can only be user accessible if in lower half, otherwise it can be modified by unrelated tasks/processes
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            )
            .map_err(|_| StackAllocationError::OutOfPhysicalMemory)?;
        let rsp = mapped_segment.start + mapped_segment.len;
        Ok(Self {
            segment,
            mapped_segment,
            rsp,
        })
    }
}

impl HigherHalfStack {
    #[must_use]
    pub fn initial_rsp(&self) -> VirtAddr {
        self.rsp
    }

    /// Returns the segment of the guard page, which is the lowest page of the stack segment.
    #[must_use]
    pub fn guard_page(&self) -> Segment {
        Segment::new(self.segment.start, Size4KiB::SIZE)
    }

    /// Returns the full stack segment, including the guard page (which is not mapped).
    pub fn segment(&self) -> &OwnedSegment<'_> {
        &self.segment
    }

    /// Returns the mapped segment, which is the part of the stack that is actually mapped in memory.
    #[must_use]
    pub fn mapped_segment(&self) -> Segment {
        self.mapped_segment
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
