#![no_std]
extern crate alloc;

pub mod access;
pub mod exec;
pub mod fcntl;
pub mod mman;
pub mod poll;
pub mod signal;
pub mod socket;
pub mod thread;
pub mod tls;
pub mod unistd;
pub mod wait;

mod ptr;
pub use ptr::*;
