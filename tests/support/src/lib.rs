//! Host-side support for the QEMU integration tests.
//!
//! Image assembly lives in `//bazel/rules:image.bzl`. Every test receives a
//! finished ISO and a finished ext2 disk through runfiles, so this crate only
//! boots them and reads the serial transcript back.

pub mod harness;

pub use harness::{HostEnv, KernelTest, Outcome, RunReport, SpawnedProcess};
