//! Vertex pulling helpers for compute-based pipeline stages.
//!
//! This is primarily used by geometry shader (GS) emulation. WebGPU does not expose a GS stage, so
//! the compat path runs VS/GS work in compute and must load vertex attributes directly from the
//! bound vertex buffers using the D3D input layout (ILAY) + vertex shader input signature.

use core::fmt;
use std::collections::HashMap;

use crate::input_layout::{
    InputLayoutDesc, SignatureSemanticKey, VsInputSignatureElement, D3D11_APPEND_ALIGNED_ELEMENT,
    MAX_INPUT_SLOTS,
};

/// A single vertex attribute pull from a vertex buffer slot into a VS input register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexPull {
    pub semantic_name_hash: u32,
    pub semantic_index: u32,
    /// D3D VS input register index (`v#`).
    pub input_register: u32,
    /// Component mask from the DXBC signature (`x=1, y=2, z=4, w=8`).
    pub mask: u8,
    /// Numeric `DXGI_FORMAT` value.
    pub dxgi_format: u32,
    /// Byte offset within the vertex, after resolving `D3D11_APPEND_ALIGNED_ELEMENT`.
    pub byte_offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexPullPlan {
    pub slot: u32,
    pub stride_bytes: u32,
    pub base_offset_bytes: u32,
    pub required_stride_bytes: u32,
    pub pulls: Vec<VertexPull>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VertexPullPlanError {
    InputSlotOutOfRange {
        slot: u32,
        max: u32,
    },
    MissingSemantic {
        semantic_name_hash: u32,
        semantic_index: u32,
    },
    UnsupportedDxgiFormat(u32),
    /// Only per-vertex elements (`input_slot_class == 0`) are supported for now.
    UnsupportedInputSlotClass(u32),
    InvalidInstanceStepRate(u32),
    MissingSlotStride {
        slot: u32,
    },
    StrideTooSmall {
        slot: u32,
        stride: u32,
        required: u32,
    },
    MisalignedOffset {
        slot: u32,
        offset: u32,
        alignment: u32,
    },
    OffsetOverflow {
        slot: u32,
        offset: u32,
        size: u32,
    },
}

impl fmt::Display for VertexPullPlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VertexPullPlanError::InputSlotOutOfRange { slot, max } => {
                write!(f, "input_slot {slot} is out of range (max {max})")
            }
            VertexPullPlanError::MissingSemantic {
                semantic_name_hash,
                semantic_index,
            } => write!(
                f,
                "missing semantic hash=0x{semantic_name_hash:08X} index={semantic_index} in VS signature"
            ),
            VertexPullPlanError::UnsupportedDxgiFormat(fmt_u32) => {
                write!(f, "unsupported DXGI_FORMAT {fmt_u32}")
            }
            VertexPullPlanError::UnsupportedInputSlotClass(class) => {
                write!(f, "unsupported input_slot_class {class}")
            }
            VertexPullPlanError::InvalidInstanceStepRate(rate) => {
                write!(f, "invalid instance_data_step_rate {rate}")
            }
            VertexPullPlanError::MissingSlotStride { slot } => {
                write!(f, "missing vertex buffer stride for slot {slot}")
            }
            VertexPullPlanError::StrideTooSmall {
                slot,
                stride,
                required,
            } => write!(
                f,
                "vertex buffer stride {stride} for slot {slot} is too small (requires at least {required})"
            ),
            VertexPullPlanError::MisalignedOffset {
                slot,
                offset,
                alignment,
            } => write!(
                f,
                "vertex element offset {offset} in slot {slot} is not aligned to {alignment} bytes"
            ),
            VertexPullPlanError::OffsetOverflow { slot, offset, size } => write!(
                f,
                "vertex element in slot {slot} has offset {offset} + size {size} which overflows u32"
            ),
        }
    }
}

impl std::error::Error for VertexPullPlanError {}

#[derive(Debug, Clone, Copy)]
struct FormatInfo {
    size: u32,
    align: u32,
}

fn dxgi_format_info(dxgi_format: u32) -> Result<FormatInfo, VertexPullPlanError> {
    // Numeric values are `DXGI_FORMAT` from dxgiformat.h.
    Ok(match dxgi_format {
        // DXGI_FORMAT_R32G32B32A32_FLOAT
        2 => FormatInfo { size: 16, align: 4 },
        // DXGI_FORMAT_R32G32B32_FLOAT
        6 => FormatInfo { size: 12, align: 4 },
        // DXGI_FORMAT_R32G32_FLOAT
        16 => FormatInfo { size: 8, align: 4 },
        _ => return Err(VertexPullPlanError::UnsupportedDxgiFormat(dxgi_format)),
    })
}

fn align_up(v: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (v + (align - 1)) & !(align - 1)
}

/// Build a vertex pulling plan for a single vertex buffer slot.
///
/// This resolves `D3D11_APPEND_ALIGNED_ELEMENT` offsets and maps ILAY semantics to VS input
/// registers using the vertex shader input signature.
pub fn build_vertex_pull_plan(
    layout: &InputLayoutDesc,
    vs_signature: &[VsInputSignatureElement],
    slot: u32,
    stride_bytes: u32,
    base_offset_bytes: u32,
) -> Result<VertexPullPlan, VertexPullPlanError> {
    if slot >= MAX_INPUT_SLOTS {
        return Err(VertexPullPlanError::InputSlotOutOfRange {
            slot,
            max: MAX_INPUT_SLOTS - 1,
        });
    }

    let mut sig_map = HashMap::<SignatureSemanticKey, (u32, u8)>::with_capacity(vs_signature.len());
    for s in vs_signature {
        sig_map.insert(
            SignatureSemanticKey {
                semantic_name_hash: s.semantic_name_hash,
                semantic_index: s.semantic_index,
            },
            (s.input_register, s.mask),
        );
    }

    let mut next_offsets = vec![0u32; MAX_INPUT_SLOTS as usize];
    let mut required_strides = vec![0u32; MAX_INPUT_SLOTS as usize];
    let mut pulls: Vec<VertexPull> = Vec::new();

    for elem in &layout.elements {
        if elem.input_slot >= MAX_INPUT_SLOTS {
            return Err(VertexPullPlanError::InputSlotOutOfRange {
                slot: elem.input_slot,
                max: MAX_INPUT_SLOTS - 1,
            });
        }

        match elem.input_slot_class {
            0 => {}
            1 => {
                if elem.instance_data_step_rate == 0 {
                    return Err(VertexPullPlanError::InvalidInstanceStepRate(
                        elem.instance_data_step_rate,
                    ));
                }
                return Err(VertexPullPlanError::UnsupportedInputSlotClass(
                    elem.input_slot_class,
                ));
            }
            other => return Err(VertexPullPlanError::UnsupportedInputSlotClass(other)),
        }

        let fmt = dxgi_format_info(elem.dxgi_format)?;
        let slot_next = &mut next_offsets[elem.input_slot as usize];
        let offset = if elem.aligned_byte_offset == D3D11_APPEND_ALIGNED_ELEMENT {
            align_up(*slot_next, fmt.align)
        } else {
            elem.aligned_byte_offset
        };
        if offset % fmt.align != 0 {
            return Err(VertexPullPlanError::MisalignedOffset {
                slot: elem.input_slot,
                offset,
                alignment: fmt.align,
            });
        }
        let end = offset
            .checked_add(fmt.size)
            .ok_or(VertexPullPlanError::OffsetOverflow {
                slot: elem.input_slot,
                offset,
                size: fmt.size,
            })?;
        *slot_next = end;
        required_strides[elem.input_slot as usize] =
            required_strides[elem.input_slot as usize].max(end);

        if elem.input_slot != slot {
            continue;
        }

        let key = SignatureSemanticKey {
            semantic_name_hash: elem.semantic_name_hash,
            semantic_index: elem.semantic_index,
        };
        let (input_register, mask) =
            sig_map
                .get(&key)
                .copied()
                .ok_or(VertexPullPlanError::MissingSemantic {
                    semantic_name_hash: elem.semantic_name_hash,
                    semantic_index: elem.semantic_index,
                })?;

        pulls.push(VertexPull {
            semantic_name_hash: elem.semantic_name_hash,
            semantic_index: elem.semantic_index,
            input_register,
            mask,
            dxgi_format: elem.dxgi_format,
            byte_offset: offset,
        });
    }

    let required_stride_bytes = required_strides[slot as usize];
    if required_stride_bytes != 0 && stride_bytes == 0 {
        return Err(VertexPullPlanError::MissingSlotStride { slot });
    }
    if stride_bytes < required_stride_bytes {
        return Err(VertexPullPlanError::StrideTooSmall {
            slot,
            stride: stride_bytes,
            required: required_stride_bytes,
        });
    }

    Ok(VertexPullPlan {
        slot,
        stride_bytes,
        base_offset_bytes,
        required_stride_bytes,
        pulls,
    })
}

/// WGSL helpers for loading vertex attributes from storage buffers.
///
/// The helpers assume little-endian packing and operate on a raw `array<u32>` view of the vertex
/// buffer. They return zero for out-of-bounds reads.
pub const WGSL_VERTEX_PULLING_HELPERS: &str = r#"
fn aero_load_u32_le(buf: ptr<storage, array<u32>, read>, byte_offset: u32) -> u32 {
  let word_idx = byte_offset >> 2u;
  let word_count = arrayLength(buf);
  if (word_idx >= word_count) {
    return 0u;
  }

  // Fast path: 4-byte aligned load.
  if ((byte_offset & 3u) == 0u) {
    return (*buf)[word_idx];
  }

  // Safe path: stitch two adjacent u32 values.
  let lo = (*buf)[word_idx];
  let hi = select(0u, (*buf)[word_idx + 1u], (word_idx + 1u) < word_count);
  let shift = (byte_offset & 3u) * 8u;
  return (lo >> shift) | (hi << (32u - shift));
}

fn aero_load_f32(buf: ptr<storage, array<u32>, read>, byte_offset: u32) -> f32 {
  return bitcast<f32>(aero_load_u32_le(buf, byte_offset));
}

fn aero_load_vec2_f32(buf: ptr<storage, array<u32>, read>, byte_offset: u32) -> vec2<f32> {
  return vec2<f32>(
    aero_load_f32(buf, byte_offset + 0u),
    aero_load_f32(buf, byte_offset + 4u),
  );
}

fn aero_load_vec3_f32(buf: ptr<storage, array<u32>, read>, byte_offset: u32) -> vec3<f32> {
  return vec3<f32>(
    aero_load_f32(buf, byte_offset + 0u),
    aero_load_f32(buf, byte_offset + 4u),
    aero_load_f32(buf, byte_offset + 8u),
  );
}

fn aero_load_vec4_f32(buf: ptr<storage, array<u32>, read>, byte_offset: u32) -> vec4<f32> {
  return vec4<f32>(
    aero_load_f32(buf, byte_offset + 0u),
    aero_load_f32(buf, byte_offset + 4u),
    aero_load_f32(buf, byte_offset + 8u),
    aero_load_f32(buf, byte_offset + 12u),
  );
}

fn aero_vec4_component(v: vec4<f32>, idx: u32) -> f32 {
  if (idx == 0u) { return v.x; }
  if (idx == 1u) { return v.y; }
  if (idx == 2u) { return v.z; }
  return v.w;
}

// Applies a D3D signature mask (x=1, y=2, z=4, w=8) to a destination register value.
//
// The DXBC signature mask indicates both which components are written and their placement within
// the packed input register. When the mask is non-contiguous (e.g. `.zw`), the incoming values are
// packed into the set bits in x/y/z/w order.
fn aero_apply_mask(prev: vec4<f32>, mask: u32, src: vec4<f32>) -> vec4<f32> {
  var out = prev;
  var src_idx: u32 = 0u;
  if ((mask & 1u) != 0u) { out.x = aero_vec4_component(src, src_idx); src_idx += 1u; }
  if ((mask & 2u) != 0u) { out.y = aero_vec4_component(src, src_idx); src_idx += 1u; }
  if ((mask & 4u) != 0u) { out.z = aero_vec4_component(src, src_idx); src_idx += 1u; }
  if ((mask & 8u) != 0u) { out.w = aero_vec4_component(src, src_idx); src_idx += 1u; }
  return out;
}
"#;

/// Emit WGSL declarations for a constant vertex pull table.
///
/// The returned string defines:
/// - `struct AeroVertexPull`
/// - `const AERO_VERTEX_PULL_COUNT: u32`
/// - `const AERO_VERTEX_PULLS: array<AeroVertexPull, N>`
pub fn emit_wgsl_pull_table(pulls: &[VertexPull]) -> String {
    let mut out = String::new();
    out.push_str("struct AeroVertexPull { reg: u32, mask: u32, fmt: u32, offset: u32 };\n");
    out.push_str(&format!(
        "const AERO_VERTEX_PULL_COUNT: u32 = {}u;\n",
        pulls.len()
    ));
    out.push_str(&format!(
        "const AERO_VERTEX_PULLS: array<AeroVertexPull, {}> = array<AeroVertexPull, {}>(\n",
        pulls.len(),
        pulls.len()
    ));
    for p in pulls {
        out.push_str(&format!(
            "  AeroVertexPull(reg: {}u, mask: {}u, fmt: {}u, offset: {}u),\n",
            p.input_register,
            u32::from(p.mask),
            p.dxgi_format,
            p.byte_offset
        ));
    }
    out.push_str(");\n");
    out
}
