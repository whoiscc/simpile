#![no_std]
#![warn(unsafe_op_in_unsafe_fn)]

pub mod fuzz;
pub mod linked;
pub mod space;

pub use space::Space;

#[cfg(test)]
extern crate std;
