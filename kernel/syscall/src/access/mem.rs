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
    /// Returns the address at which this mapping exists.
    fn addr(&self) -> UserspacePtr<u8>;

    /// Returns the length of this mapping.
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
    ) -> Result<Self::Mapping, CreateMappingError>;
}
