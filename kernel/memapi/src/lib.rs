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

/// Allocates mapped memory. Dropping an allocation frees it.
pub trait MemoryApi {
    type AllocOptions;
    type ReadonlyAllocation: Allocation;
    type WritableAllocation: WritableAllocation;
    type ExecutableAllocation: Allocation;

    /// Allocates memory at the given location with the given layout. `options`
    /// carries implementation defined properties of the allocation, such as
    /// whether the memory is reachable from user space.
    ///
    /// The returned memory is writable and *not* executable until converted
    /// with [`MemoryApi::make_executable`].
    ///
    /// A guarded allocation has a guard page on either end that faults when
    /// accessed.
    ///
    /// Returns `None` if the allocation cannot be created, for example because
    /// the requested size is too large or the location is invalid.
    fn allocate(
        &mut self,
        location: Location,
        layout: Layout,
        options: Self::AllocOptions,
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
