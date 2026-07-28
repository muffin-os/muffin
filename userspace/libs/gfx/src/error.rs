/// Convenience alias for a [`core::result::Result`] with [`GfxError`].
pub type Result<T> = core::result::Result<T, GfxError>;

/// Error type for graphics operations.
#[derive(Debug)]
pub enum GfxError {
    /// A shader was passed to the wrong pipeline slot (e.g. a fragment shader
    /// where a vertex shader was expected).
    WrongShaderKind,
}
