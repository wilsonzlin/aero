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
/// Parsers for DXBC signature chunks (`ISGN`, `OSGN`, `PSGN`, ...).
pub mod signature;
/// Parsers for SM4/SM5 shader bytecode chunks (`SHDR`/`SHEX`).
pub mod sm4;

/// Helpers for building synthetic DXBC blobs in tests.
///
/// This module is only available when compiling this crate's own tests, or when
/// the `test-utils` feature is enabled. It is **not** considered part of the
/// stable parsing API.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

pub use crate::dxbc::{DxbcChunk, DxbcFile, DxbcHeader};
pub use crate::error::DxbcError;
pub use crate::fourcc::FourCC;
pub use crate::signature::{
    parse_signature_chunk, parse_signature_chunk_with_fourcc, SignatureChunk, SignatureEntry,
};
pub use crate::sm4::{decode_version_token, ShaderModel, ShaderStage, Sm4Error, Sm4Program, Sm5Program};
