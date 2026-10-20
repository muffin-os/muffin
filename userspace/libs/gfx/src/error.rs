/// Convenience alias for a [`core::result::Result`] with [`GfxError`].
pub type Result<T> = core::result::Result<T, GfxError>;

/// Error type for graphics operations.
#[derive(Debug, PartialEq, Eq)]
pub enum GfxError {
    /// A shader was passed to the wrong pipeline slot (e.g. a fragment shader
    /// where a vertex shader was expected).
    WrongShaderKind,
    /// A pipeline's `vertex_stride` is zero or not a whole number of
    /// attributes.
    InvalidVertexStride,
    /// A vertex shader's `output_count` leaves no room for the position.
    InvalidOutputCount,
    /// `draw` was recorded before `bind_pipeline`.
    PipelineNotBound,
    /// `draw` was recorded before `bind_vertex_buffer`.
    VertexBufferNotBound,
}
