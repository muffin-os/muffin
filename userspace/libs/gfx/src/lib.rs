//! Graphics subsystem for the muffin kernel.
//!
//! Provides driver-agnostic traits for querying adapter capabilities,
//! allocating GPU resources, recording draw commands, and presenting
//! frames. A pure-software fallback backend is included under
//! `backend::software`.

#![no_std]
extern crate alloc;

pub mod api;
pub mod backend;
pub mod error;
