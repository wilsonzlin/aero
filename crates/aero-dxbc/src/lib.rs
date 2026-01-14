//! A safe, zero-copy parser for DirectX shader bytecode containers (`DXBC`).
//!
//! This crate is intended for parsing **untrusted** shader blobs (e.g. from guest
//! memory) without panicking or reading out of bounds.
//!
//! In addition to container parsing, this crate also provides:
//!
//! - A safe parser for D3D10+ signature chunks (`ISGN`/`OSGN`/`PSGN` and variants),
//!   which are needed to map shader inputs/outputs to registers.
//! - Minimal parsers for common reflection-related chunks:
//!   - `RDEF` (bound resources + binding points)
//!   - `CTAB` (legacy constant register ranges)
//! - A bounds-checked SM4/SM5 token stream extractor for `SHDR`/`SHEX` chunks.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod dxbc;
mod error;
mod fourcc;
/// Parser for DXBC resource definition chunks (`RDEF`).
pub mod rdef;
/// Parsers for DXBC signature chunks (`ISGN`, `OSGN`, `PSGN`, ...).
pub mod signature;
/// Parsers for SM4/SM5 shader bytecode chunks (`SHDR`/`SHEX`).
pub mod sm4;
/// Parsers for legacy Direct3D constant table chunks (`CTAB`).
pub mod ctab;

/// Helpers for building synthetic DXBC blobs in tests.
///
/// This module is only available when compiling this crate's own tests, or when
/// the `test-utils` feature is enabled. It is **not** considered part of the
/// stable parsing API.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
/// Advanced DXBC parsing helpers (reflection, disassembly, shader model extraction).
///
/// This module is gated behind the `robust` feature because it provides
/// higher-level parsing helpers that allocate and are not required for all
/// consumers.
#[cfg(feature = "robust")]
pub mod robust;

#[cfg(test)]
mod tests;

pub use crate::dxbc::{DxbcChunk, DxbcFile, DxbcHeader};
pub use crate::error::DxbcError;
pub use crate::fourcc::FourCC;
pub use crate::rdef::{
    parse_rdef_chunk, parse_rdef_chunk_for_fourcc, parse_rdef_chunk_with_fourcc, RdefChunk,
    RdefConstantBuffer, RdefResourceBinding, RdefStructMember, RdefType, RdefVariable,
};
pub use crate::signature::{
    parse_signature_chunk, parse_signature_chunk_with_fourcc, SignatureChunk, SignatureEntry,
};
pub use crate::sm4::{
    decode_version_token, ShaderModel, ShaderStage, Sm4Error, Sm4Program, Sm5Program,
};
pub use crate::ctab::{parse_ctab_chunk, ConstantTable, CtabConstant};
