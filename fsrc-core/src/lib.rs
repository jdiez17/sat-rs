//! # Core components of the Flight Software Rust Crate (FSRC) collection
//!
//! This is a collection of Rust crates which can be used to build On-Board Software for remote
//! systems like satellites of rovers. It has special support for space tailored protocols
//! like the ones provided by CCSDS and ECSS.
//!
//! The crates can generally be used in a `no_std` environment, but some crates expect that the
//! [alloc](https://doc.rust-lang.org/alloc) crate is available to allow boxed trait objects.
//! These are used to supply user code to the crates.
#![no_std]
#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

pub mod error;
#[cfg(feature = "alloc")]
pub mod event_man;
pub mod events;
#[cfg(feature = "std")]
pub mod executable;
pub mod hal;
pub mod objects;
#[cfg(feature = "alloc")]
pub mod pool;
pub mod tmtc;

extern crate downcast_rs;
