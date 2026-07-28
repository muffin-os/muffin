use crate::api::GfxFence;

/// Software fence; immediately signals completion because all execution
/// is synchronous.
pub struct SoftFence;

impl GfxFence for SoftFence {
    fn is_ready(&self) -> bool {
        true
    }

    fn wait(&self) {}
}
