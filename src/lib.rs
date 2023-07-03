#![no_std]
#![warn(unsafe_op_in_unsafe_fn)]


pub mod linked;
pub mod space;

pub use space::Space;

#[cfg(any(feature = "std", test))]
pub mod fuzz;
#[cfg(feature = "switchable")]
pub mod switchable;
#[cfg(feature = "switchable")]
pub use switchable::Switchable;

#[cfg(any(feature = "std", feature = "switchable", test))]
extern crate std;
