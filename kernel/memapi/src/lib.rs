#![no_std]
extern crate alloc;

use core::alloc::Layout;

use x86_64::VirtAddr;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Location {
    Anywhere,
    Fixed(VirtAddr),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum UserAccessible {
    No,
    Yes,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Guarded {
    No,
    Yes,
}

/// This functions as an API type that components can require if they need
/// to allocate mapped memory. When invoking the component, the kernel will pass
/// a concrete implementation of this trait.
///
/// The [`MemoryApi::Allocation`] type is a type that will free the allocated memory
/// upon drop.
pub trait MemoryApi {
    type ReadonlyAllocation: Allocation;
    type WritableAllocation: WritableAllocation;
    type ExecutableAllocation: Allocation;

    /// Allocates memory at the given location with the given layout (size and align).
    /// If the allocation should be accessible from user space, the caller must pass
    /// `UserAccessible::Yes`.
    /// By default, the returned memory region is writable and *not* executable.
    /// To change this, the caller must convert the allocation with [`MemoryApi::make_executable`].
    ///
    /// If the allocation should be guarded, then the implementation must ensure that on the lower
    /// and higher end of the allocation, there is a guard page that will cause a page fault when
    /// accessed.
    ///
    /// The implementation chooses the physical addresses, the address space and the memory region,
    /// which is opaque to the caller of this function.
    ///
    /// This function must return `None` if the allocation cannot be created, for example, because
    /// the requested size is too large or the location is invalid.
    ///
    /// After successful creation, the caller of this function owns the allocated memory. He can free
    /// the memory by dropping the returned [`MemoryApi::Allocation`] type.
    fn allocate(
        &mut self,
        location: Location,
        layout: Layout,
        user_accessible: UserAccessible,
        guarded: Guarded,
    ) -> Option<Self::WritableAllocation>;

    /// # Errors
    /// Returns an error if the allocation cannot be converted into an executable allocation.
    fn make_executable(
        &mut self,
        allocation: Self::WritableAllocation,
    ) -> Result<Self::ExecutableAllocation, Self::WritableAllocation>;

    /// # Errors
    /// Returns an error if the allocation cannot be converted into a writable allocation.
    fn make_writable(
        &mut self,
        allocation: Self::ExecutableAllocation,
    ) -> Result<Self::WritableAllocation, Self::ExecutableAllocation>;

    /// # Errors
    /// Returns an error if the allocation cannot be converted into a readonly allocation.
    fn make_readonly(
        &mut self,
        allocation: Self::WritableAllocation,
    ) -> Result<Self::ReadonlyAllocation, Self::WritableAllocation>;
}

/// A segment of memory that can be read from. Use the [`AsRef`] trait to access
/// the memory.
pub trait Allocation: AsRef<[u8]> {
    fn layout(&self) -> Layout;
}

/// A segment of memory that can be written to. Use the [`AsMut`] trait to access
/// the memory mutably.
pub trait WritableAllocation: Allocation + AsMut<[u8]> {}
