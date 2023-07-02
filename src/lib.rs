#![no_std]
#![warn(unsafe_op_in_unsafe_fn)]


pub mod linked;
pub mod space;

#[cfg(any(feature = "std", test))]
pub mod fuzz;

pub use space::Space;

#[cfg(any(feature = "std", test))]
extern crate std;
