#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stat {
    pub size: usize,
    /// POSIX `st_mode`, see the `S_IF*` constants in `kernel_abi`.
    pub mode: u32,
}
