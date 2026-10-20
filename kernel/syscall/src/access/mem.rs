use crate::UserspacePtr;

pub enum AllocationStrategy {
    Eager,
    Lazy,
}

pub enum Location {
    Anywhere,
    Fixed(UserspacePtr<u8>),
}

pub enum CreateMappingError {
    LocationAlreadyMapped,
    OutOfMemory,
}
