use kernel_abi::ProtFlags;

use crate::UserspacePtr;

pub enum AllocationStrategy {
    Eager,
    Lazy,
}

pub enum Location {
    Anywhere,
    Fixed(UserspacePtr<u8>),
}

pub trait Mapping {
    fn addr(&self) -> UserspacePtr<u8>;

    /// Size in bytes.
    fn size(&self) -> usize;
}

pub enum CreateMappingError {
    LocationAlreadyMapped,
    OutOfMemory,
}

pub trait MemoryAccess {
    type Mapping: Mapping;

    fn create_mapping(
        &self,
        location: Location,
        size: usize,
        allocation_strategy: AllocationStrategy,
        prot: ProtFlags,
    ) -> Result<Self::Mapping, CreateMappingError>;
}
