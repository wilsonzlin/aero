//! WGSL helper library + binding scheme for compute-based vertex pulling from the D3D input
//! assembler (IA) vertex buffers.
//!
//! This is intended for internal compute prepasses (e.g. GS/HS/DS emulation) that need to read
//! vertex attributes directly out of the D3D11 IA buffers using "vertex pulling".
//!
//! The corresponding WGSL (`vertex_pulling.wgsl`) defines:
//! - `@group(2) @binding(0)` = uniform metadata (`IaMeta`)
//! - `@group(2) @binding(1..=8)` = up to 8 IA vertex buffers as `var<storage, read>`
//!
//! The metadata encodes per-slot base offset + stride in bytes.

/// Current WebGPU baseline minimum for vertex buffer slots.
///
/// D3D11 exposes 32 IA slots, but `aero-d3d11` compacts them down to at most 8 for WebGPU.
pub const IA_MAX_VERTEX_BUFFERS: usize = 8;

/// Bind group binding index for [`IaMeta`] (`var<uniform>`).
pub const IA_BINDING_META: u32 = 0;

/// Base bind group binding index for `ia_vbN` (`var<storage, read>`).
pub const IA_BINDING_VERTEX_BUFFER_BASE: u32 = 1;

/// Bind group binding index for `ia_vb0` (`var<storage, read>`).
pub const IA_BINDING_VERTEX_BUFFER0: u32 = IA_BINDING_VERTEX_BUFFER_BASE;

/// Bind group binding index for the first binding after the `ia_vbN` range.
pub const IA_BINDING_VERTEX_BUFFER_END: u32 =
    IA_BINDING_VERTEX_BUFFER_BASE + IA_MAX_VERTEX_BUFFERS as u32;

/// WGSL source for the IA vertex pulling helper library.
pub const WGSL: &str = include_str!("vertex_pulling.wgsl");

/// Per-vertex-buffer metadata entry.
///
/// Layout matches the WGSL `vec4<u32>` used in `IaMeta.vb[]`:
/// - `.x` = `base_offset_bytes`
/// - `.y` = `stride_bytes`
/// - `.z/.w` = reserved (padding)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct IaVertexBufferMeta {
    pub base_offset_bytes: u32,
    pub stride_bytes: u32,
    pub _reserved: [u32; 2],
}

/// Uniform buffer payload for IA vertex pulling.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct IaMeta {
    pub vb: [IaVertexBufferMeta; IA_MAX_VERTEX_BUFFERS],
}

impl IaMeta {
    /// Returns the bytes for uploading into a `wgpu::Buffer` with `UNIFORM` usage.
    pub fn as_bytes(&self) -> &[u8] {
        // Safety: `Self` is `#[repr(C)]` and contains only plain-old-data.
        unsafe {
            std::slice::from_raw_parts(
                (self as *const Self).cast::<u8>(),
                std::mem::size_of::<Self>(),
            )
        }
    }
}
