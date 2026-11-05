use crate::api::GfxFence;

/// Software fence; immediately signals completion because all execution
/// is synchronous.
#[derive(Debug, PartialEq, Eq)]
pub struct SoftFence;

impl GfxFence for SoftFence {
    fn is_ready(&self) -> bool {
        true
    }

    fn wait(&self) {}

    fn id(&self) -> u64 {
        0
    }
}
