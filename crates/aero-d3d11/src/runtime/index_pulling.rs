//! WGSL + runtime helpers for "index pulling" in compute-based draw expansion.
//!
//! WebGPU's render pipeline handles indexed draws implicitly via `set_index_buffer` +
//! `draw_indexed`, but compute-based prepasses (e.g. geometry/tessellation emulation) need to
//! interpret the indexed draw stream manually:
//! - read u16/u32 indices from the bound index buffer,
//! - apply `first_index`,
//! - apply `base_vertex` (signed),
//! - and typically fan this out across instances.
//!
//! This module provides:
//! - A tiny WGSL snippet that implements index-buffer reads and vertex-id resolution.
//! - A packed uniform payload used by the runtime to pass `{first_index, base_vertex, index_format}`.
//!
//! Note: the uniform intentionally contains only the fields that are required for resolving vertex
//! IDs. Other draw parameters (index/vertex counts, first_instance, etc) are expected to be handled
//! by the caller/dispatcher.

use crate::input_layout::MAX_WGPU_VERTEX_BUFFERS;
use super::vertex_pulling::VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE;

/// `IndexPullingParams.index_format` value for 16-bit indices.
pub const INDEX_FORMAT_U16: u32 = 0;
/// `IndexPullingParams.index_format` value for 32-bit indices.
pub const INDEX_FORMAT_U32: u32 = 1;

/// Canonical `@binding` number (within the chosen bind group) for [`IndexPullingParams`].
///
/// When used alongside [`crate::runtime::vertex_pulling`], this is intended to live in
/// [`crate::runtime::vertex_pulling::VERTEX_PULLING_GROUP`] in the same reserved internal binding
/// range as vertex pulling itself (to avoid collisions with D3D `b#`/`t#`/`s#`/`u#` bindings when
/// sharing a bind group).
///
/// It is placed immediately after the maximum possible vertex-buffer bindings, i.e. after
/// `VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + MAX_WGPU_VERTEX_BUFFERS - 1`.
pub const INDEX_PULLING_PARAMS_BINDING: u32 =
    VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + MAX_WGPU_VERTEX_BUFFERS;

/// Canonical `@binding` number (within the chosen bind group) for the index buffer (`array<u32>`).
///
/// See [`INDEX_PULLING_PARAMS_BINDING`] for the intended pairing with vertex pulling.
pub const INDEX_PULLING_BUFFER_BINDING: u32 = INDEX_PULLING_PARAMS_BINDING + 1;

/// Uniform payload for compute-based index pulling.
///
/// Layout is WGSL-compatible and padded to 16 bytes so it can be bound as a WebGPU uniform buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexPullingParams {
    pub first_index: u32,
    pub base_vertex: i32,
    pub index_format: u32,
    pub _pad0: u32,
}

impl IndexPullingParams {
    /// Serializes this struct into little-endian bytes suitable for `Queue::write_buffer`.
    pub fn to_le_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.first_index.to_le_bytes());
        out[4..8].copy_from_slice(&self.base_vertex.to_le_bytes());
        out[8..12].copy_from_slice(&self.index_format.to_le_bytes());
        out[12..16].copy_from_slice(&self._pad0.to_le_bytes());
        out
    }
}

/// Builds a WGSL snippet implementing index pulling.
///
/// The generated WGSL declares two bindings:
/// - `@group(group) @binding(params_binding)` uniform `IndexPullingParams`
/// - `@group(group) @binding(index_buffer_binding)` storage `array<u32>` containing the index data
///
/// The index buffer is modelled as `array<u32>` for two reasons:
/// - u32 indices are naturally addressed as `index_words[idx]`
/// - u16 indices are unpacked from 32-bit words using shifts/masks
///
/// The snippet exposes:
/// - `index_pulling_load_index_u32(abs_index: u32) -> u32`
/// - `index_pulling_resolve_vertex_id(index_in_draw: u32) -> i32`
pub fn wgsl_index_pulling_lib(group: u32, params_binding: u32, index_buffer_binding: u32) -> String {
    format!(
        r#"
struct IndexPullingParams {{
    first_index: u32,
    base_vertex: i32,
    index_format: u32,
    _pad0: u32,
}};

@group({group}) @binding({params_binding})
var<uniform> index_pulling_params: IndexPullingParams;

// Index buffer exposed as 32-bit words.
@group({group}) @binding({index_buffer_binding})
var<storage, read> index_pulling_words: array<u32>;

fn index_pulling_load_index_u32(abs_index: u32) -> u32 {{
    if (index_pulling_params.index_format == {INDEX_FORMAT_U16}u) {{
        let word = index_pulling_words[abs_index >> 1u];
        let shift = (abs_index & 1u) * 16u;
        return (word >> shift) & 0xFFFFu;
    }}
    return index_pulling_words[abs_index];
}}

fn index_pulling_resolve_vertex_id(index_in_draw: u32) -> i32 {{
    let abs_index = index_in_draw + index_pulling_params.first_index;
    let idx = index_pulling_load_index_u32(abs_index);
    return i32(idx) + index_pulling_params.base_vertex;
}}
"#
    )
}
