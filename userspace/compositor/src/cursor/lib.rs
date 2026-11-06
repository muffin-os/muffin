//! This crate is the mouse cursor of the compositor. It holds the pointer
//! state and the arrow that shows it. It has no device code, so it builds and
//! tests on the host.

pub mod arrow;
pub mod pointer;
