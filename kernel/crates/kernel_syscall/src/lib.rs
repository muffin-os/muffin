#![no_std]
#![feature(negative_impls)]
extern crate alloc;

pub mod access;
pub mod fcntl;
pub mod mman;
pub mod unistd;

mod ptr;
pub use ptr::*;
