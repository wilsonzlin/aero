//! A safe, zero-copy parser for DirectX shader bytecode containers (`DXBC`).
//!
//! This crate is intended for parsing **untrusted** shader blobs (e.g. from guest
//! memory) without panicking or reading out of bounds.
//!
//! In addition to container parsing, this crate also provides a safe parser for
//! D3D10+ signature chunks (`ISGN`/`OSGN`/`PSGN` and variants), which are needed
//! to map shader inputs/outputs to registers.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod dxbc;
mod error;
mod fourcc;
mod signature;

pub use crate::dxbc::{DxbcChunk, DxbcFile, DxbcHeader};
pub use crate::error::DxbcError;
pub use crate::fourcc::FourCC;
pub use crate::signature::{parse_signature_chunk, SignatureChunk, SignatureEntry};
