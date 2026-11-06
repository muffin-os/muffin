#![no_std]
#![feature(negative_impls)]
extern crate alloc;

pub mod access;
pub mod exec;
pub mod fcntl;
pub mod mman;
pub mod poll;
pub mod signal;
pub mod socket;
pub mod unistd;
pub mod wait;

mod ptr;
pub use ptr::*;
