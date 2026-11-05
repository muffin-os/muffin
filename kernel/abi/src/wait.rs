use bitflags::bitflags;

bitflags! {
    /// The `options` argument of waitpid.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct WaitOptions: i32 {
        /// Return 0 at once when no selected child has exited.
        const NOHANG = 1;
    }
}
