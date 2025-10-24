use bitflags::bitflags;

bitflags! {
    /// Memory protection flags for mmap
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProtFlags: i32 {
        const NONE = 0x0;
        const READ = 0x1;
        const WRITE = 0x2;
        const EXEC = 0x4;
    }
}

bitflags! {
    /// Memory mapping flags for mmap
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MapFlags: i32 {
        const SHARED = 0x01;
        const PRIVATE = 0x02;
        const FIXED = 0x10;
        const ANONYMOUS = 0x20;
    }
}

impl MapFlags {
    pub const ANON: Self = Self::ANONYMOUS;
}
