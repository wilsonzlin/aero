#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "std"), no_std)]
#![doc = include_str!("../README.md")]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod io;

/// Convenience re-export so consumers can `use nt_packetlib::packet::...`.
pub use io::net::packet;
