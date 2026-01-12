//! D3D10/11 input layout (ILAY) blob parsing and mapping to WebGPU vertex buffer layouts.
//!
//! The Win7 D3D10/11 UMD emits `AEROGPU_CMD_CREATE_INPUT_LAYOUT` carrying an opaque blob.
//! For D3D10/11 the blob payload uses the `"ILAY"` format defined in
//! `drivers/aerogpu/protocol/aerogpu_cmd.h`:
//!
//! ```text
//! struct aerogpu_input_layout_blob_header
//! struct aerogpu_input_layout_element_dxgi elements[element_count]
//! ```
//!
//! WebGPU requires `wgpu::VertexBufferLayout` at pipeline creation time. D3D11's input layout
//! elements do **not** include the vertex buffer stride, so callers must combine the parsed ILAY
//! with the currently-bound IA vertex buffer strides (see [`InputLayoutBinding`]).

use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// `"ILAY"` little-endian.
pub const AEROGPU_INPUT_LAYOUT_BLOB_MAGIC: u32 = 0x5941_4C49;
pub const AEROGPU_INPUT_LAYOUT_BLOB_VERSION: u32 = 1;

/// Mirrors `D3D11_APPEND_ALIGNED_ELEMENT`.
pub const D3D11_APPEND_ALIGNED_ELEMENT: u32 = 0xFFFF_FFFF;

const HEADER_SIZE: usize = 16;
const ELEMENT_SIZE: usize = 28;

/// Conservative upper bound for ILAY element count.
///
/// D3D11 defines `D3D11_IA_VERTEX_INPUT_STRUCTURE_ELEMENT_COUNT` as 32.
pub const MAX_INPUT_LAYOUT_ELEMENTS: u32 = 32;

/// D3D11 input-assembler vertex buffer slots.
///
/// D3D11 defines `D3D11_IA_VERTEX_INPUT_RESOURCE_SLOT_COUNT` as 32.
pub const MAX_INPUT_SLOTS: u32 = 32;

/// WebGPU baseline limits (minimum required by spec).
///
/// We validate against these early so pipeline creation doesn't fail later with opaque wgpu errors.
pub const MAX_WGPU_VERTEX_BUFFERS: u32 = 8;
pub const MAX_WGPU_VERTEX_ATTRIBUTES: u32 = 16;

/// Result of mapping a D3D11 ILAY input layout to WebGPU vertex buffer layouts.
///
/// WebGPU vertex buffers are indexed 0..N. D3D11 input layouts can reference up to 32 input slots
/// and the slot indices are part of the draw-time IA state. When using
/// [`map_layout_to_shader_locations_compact`], only slots referenced by the layout are emitted and
/// a mapping from D3D slot → WebGPU slot is returned so the executor can bind the correct buffers at
/// draw time.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedInputLayout {
    /// WebGPU vertex buffer layouts in **WebGPU slot order** (0..N).
    pub buffers: Vec<VertexBufferLayoutOwned>,
    /// D3D input slot → WebGPU buffer slot.
    pub d3d_slot_to_wgpu_slot: BTreeMap<u32, u32>,
}

impl MappedInputLayout {
    /// Convenience helper for building a `wgpu::VertexState::buffers` slice.
    ///
    /// The returned layouts borrow from `self`, so `self` must outlive the pipeline descriptor
    /// creation call.
    pub fn wgpu_vertex_buffer_layouts(&self) -> Vec<wgpu::VertexBufferLayout<'_>> {
        self.buffers
            .iter()
            .map(VertexBufferLayoutOwned::as_wgpu)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLayoutBlobHeader {
    pub magic: u32,
    pub version: u32,
    pub element_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLayoutElementDxgi {
    pub semantic_name_hash: u32,
    pub semantic_index: u32,
    /// Numeric `DXGI_FORMAT` value.
    pub dxgi_format: u32,
    pub input_slot: u32,
    pub aligned_byte_offset: u32,
    /// 0 = per-vertex, 1 = per-instance.
    pub input_slot_class: u32,
    pub instance_data_step_rate: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputLayoutDesc {
    pub header: InputLayoutBlobHeader,
    pub elements: Vec<InputLayoutElementDxgi>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignatureSemanticKey {
    pub semantic_name_hash: u32,
    pub semantic_index: u32,
}

/// Minimal vertex shader input signature element needed for ILAY→location mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsInputSignatureElement {
    pub semantic_name_hash: u32,
    pub semantic_index: u32,
    pub input_register: u32,
    /// Component mask from the DXBC `ISGN` signature (`x=1, y=2, z=4, w=8`).
    ///
    /// This is informational for now (the ILAY protocol does not include per-component routing),
    /// but it is useful when signatures pack multiple semantics into the same input register.
    pub mask: u8,
    /// WGSL `@location` / `wgpu::VertexAttribute::shader_location` assigned for this semantic.
    ///
    /// Note: This can differ from [`Self::input_register`] because WebGPU requires each vertex
    /// attribute location to be unique, while D3D signatures can pack multiple semantics into a
    /// single input register (e.g. one semantic uses `.xy` and another uses `.zw` of the same
    /// register). In that case, we assign each semantic a unique `shader_location` and reconstruct
    /// the packed input register in WGSL.
    pub shader_location: u32,
}

/// Owned `wgpu::VertexBufferLayout`.
///
/// `wgpu::VertexBufferLayout<'a>` borrows the attribute slice, so we store an owned variant and
/// provide a helper to borrow it when building pipeline descriptors.
#[derive(Debug, Clone, PartialEq)]
pub struct VertexBufferLayoutOwned {
    pub array_stride: wgpu::BufferAddress,
    pub step_mode: wgpu::VertexStepMode,
    pub attributes: Vec<wgpu::VertexAttribute>,
}

impl VertexBufferLayoutOwned {
    pub fn as_wgpu(&self) -> wgpu::VertexBufferLayout<'_> {
        wgpu::VertexBufferLayout {
            array_stride: self.array_stride,
            step_mode: self.step_mode,
            attributes: &self.attributes,
        }
    }
}

/// ILAY + currently-bound vertex buffer strides.
///
/// D3D11 stores stride as part of the IA state (`IASetVertexBuffers`), not inside the input layout
/// object. WebGPU requires `array_stride` at pipeline creation time, so callers must provide the
/// bound strides for each slot referenced by the input layout.
#[derive(Debug, Clone, Copy)]
pub struct InputLayoutBinding<'a> {
    pub layout: &'a InputLayoutDesc,
    /// Strides indexed by input slot. Slots not present in the slice (or with stride 0) are
    /// considered unbound.
    pub slot_strides: &'a [u32],
}

impl<'a> InputLayoutBinding<'a> {
    pub fn new(layout: &'a InputLayoutDesc, slot_strides: &'a [u32]) -> Self {
        Self {
            layout,
            slot_strides,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputLayoutError {
    BufferTooSmall {
        expected: usize,
        actual: usize,
    },
    InvalidMagic(u32),
    InvalidVersion(u32),
    ElementCountTooLarge {
        count: u32,
        max: u32,
    },
    InputSlotOutOfRange {
        slot: u32,
        max: u32,
    },
    TooManyVertexBuffers {
        max_slot: u32,
        max: u32,
    },
    TooManyUsedVertexBuffers {
        count: u32,
        max: u32,
    },
    TooManyVertexAttributes {
        count: u32,
        max: u32,
    },
    UnsupportedDxgiFormat(u32),
    UnsupportedInputSlotClass(u32),
    InvalidInstanceStepRate(u32),
    MissingSemantic {
        semantic_name_hash: u32,
        semantic_index: u32,
    },
    DuplicateShaderLocation {
        shader_location: u32,
    },
    MixedStepModeInSlot {
        slot: u32,
        first: wgpu::VertexStepMode,
        second: wgpu::VertexStepMode,
    },
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

impl fmt::Display for InputLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputLayoutError::BufferTooSmall { expected, actual } => write!(
                f,
                "input layout blob too small: expected at least {expected} bytes, got {actual}"
            ),
            InputLayoutError::InvalidMagic(magic) => {
                write!(f, "invalid ILAY magic 0x{magic:08X}")
            }
            InputLayoutError::InvalidVersion(version) => {
                write!(f, "unsupported ILAY version {version}")
            }
            InputLayoutError::ElementCountTooLarge { count, max } => {
                write!(f, "ILAY element_count {count} exceeds max {max}")
            }
            InputLayoutError::InputSlotOutOfRange { slot, max } => {
                write!(f, "input_slot {slot} is out of range (max {max})")
            }
            InputLayoutError::TooManyVertexBuffers { max_slot, max } => write!(
                f,
                "input layout references slot {max_slot}, but WebGPU only supports {max} vertex buffers"
            ),
            InputLayoutError::TooManyUsedVertexBuffers { count, max } => write!(
                f,
                "input layout uses {count} vertex buffers, but WebGPU only supports {max}"
            ),
            InputLayoutError::TooManyVertexAttributes { count, max } => write!(
                f,
                "input layout has {count} vertex attributes, but WebGPU only supports {max}"
            ),
            InputLayoutError::UnsupportedDxgiFormat(fmt) => {
                write!(f, "unsupported DXGI_FORMAT {fmt}")
            }
            InputLayoutError::UnsupportedInputSlotClass(class) => {
                write!(f, "unsupported input_slot_class {class}")
            }
            InputLayoutError::InvalidInstanceStepRate(rate) => {
                write!(f, "invalid instance_data_step_rate {rate}")
            }
            InputLayoutError::MissingSemantic {
                semantic_name_hash,
                semantic_index,
            } => write!(
                f,
                "input layout element references missing semantic (hash=0x{semantic_name_hash:08X}, index={semantic_index})"
            ),
            InputLayoutError::DuplicateShaderLocation { shader_location } => {
                write!(f, "duplicate shader_location {shader_location}")
            }
            InputLayoutError::MixedStepModeInSlot { slot, first, second } => write!(
                f,
                "input slot {slot} mixes step modes ({first:?} vs {second:?})"
            ),
            InputLayoutError::MissingSlotStride { slot } => {
                write!(f, "missing vertex buffer stride for input slot {slot}")
            }
            InputLayoutError::StrideTooSmall {
                slot,
                stride,
                required,
            } => write!(
                f,
                "vertex buffer stride {stride} for slot {slot} is smaller than required {required}"
            ),
            InputLayoutError::MisalignedOffset {
                slot,
                offset,
                alignment,
            } => write!(
                f,
                "input layout element offset {offset} in slot {slot} is not aligned to {alignment} bytes"
            ),
            InputLayoutError::OffsetOverflow { slot, offset, size } => write!(
                f,
                "input layout element in slot {slot} has offset {offset} + size {size} which overflows u32"
            ),
        }
    }
}

impl std::error::Error for InputLayoutError {}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn align_up(v: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (v + (align - 1)) & !(align - 1)
}

impl InputLayoutDesc {
    /// Parse an ILAY input layout blob.
    pub fn parse(blob: &[u8]) -> Result<Self, InputLayoutError> {
        if blob.len() < HEADER_SIZE {
            return Err(InputLayoutError::BufferTooSmall {
                expected: HEADER_SIZE,
                actual: blob.len(),
            });
        }

        let magic = read_u32_le(&blob[0..4]);
        if magic != AEROGPU_INPUT_LAYOUT_BLOB_MAGIC {
            return Err(InputLayoutError::InvalidMagic(magic));
        }
        let version = read_u32_le(&blob[4..8]);
        if version != AEROGPU_INPUT_LAYOUT_BLOB_VERSION {
            return Err(InputLayoutError::InvalidVersion(version));
        }
        let element_count = read_u32_le(&blob[8..12]);
        if element_count > MAX_INPUT_LAYOUT_ELEMENTS {
            return Err(InputLayoutError::ElementCountTooLarge {
                count: element_count,
                max: MAX_INPUT_LAYOUT_ELEMENTS,
            });
        }

        let expected_size = HEADER_SIZE + (element_count as usize) * ELEMENT_SIZE;
        // Forward-compat: allow trailing bytes so newer ILAY blob versions can append fields after
        // the element array without breaking older hosts. Only the element prefix we understand is
        // required.
        if blob.len() < expected_size {
            return Err(InputLayoutError::BufferTooSmall {
                expected: expected_size,
                actual: blob.len(),
            });
        }

        let mut elements = Vec::with_capacity(element_count as usize);
        let mut off = HEADER_SIZE;
        for _ in 0..element_count {
            let semantic_name_hash = read_u32_le(&blob[off..off + 4]);
            let semantic_index = read_u32_le(&blob[off + 4..off + 8]);
            let dxgi_format = read_u32_le(&blob[off + 8..off + 12]);
            let input_slot = read_u32_le(&blob[off + 12..off + 16]);
            if input_slot >= MAX_INPUT_SLOTS {
                return Err(InputLayoutError::InputSlotOutOfRange {
                    slot: input_slot,
                    max: MAX_INPUT_SLOTS - 1,
                });
            }
            let aligned_byte_offset = read_u32_le(&blob[off + 16..off + 20]);
            let input_slot_class = read_u32_le(&blob[off + 20..off + 24]);
            let instance_data_step_rate = read_u32_le(&blob[off + 24..off + 28]);
            elements.push(InputLayoutElementDxgi {
                semantic_name_hash,
                semantic_index,
                dxgi_format,
                input_slot,
                aligned_byte_offset,
                input_slot_class,
                instance_data_step_rate,
            });
            off += ELEMENT_SIZE;
        }

        Ok(Self {
            header: InputLayoutBlobHeader {
                magic,
                version,
                element_count,
            },
            elements,
        })
    }
}

struct VertexFormatInfo {
    format: wgpu::VertexFormat,
    size: u32,
    /// Offset alignment used when resolving `D3D11_APPEND_ALIGNED_ELEMENT`.
    ///
    /// D3D10/11's "append aligned" rule aligns each element to a 4-byte boundary, so we keep this
    /// at 4 even for 8-bit/16-bit formats.
    align: u32,
}

fn dxgi_format_to_vertex_format(dxgi_format: u32) -> Result<VertexFormatInfo, InputLayoutError> {
    // Numeric values are `DXGI_FORMAT` from dxgiformat.h.
    Ok(match dxgi_format {
        // R32G32B32A32_FLOAT
        2 => VertexFormatInfo {
            format: wgpu::VertexFormat::Float32x4,
            size: 16,
            align: 4,
        },
        // R32G32B32_FLOAT
        6 => VertexFormatInfo {
            format: wgpu::VertexFormat::Float32x3,
            size: 12,
            align: 4,
        },
        // R32G32_FLOAT
        16 => VertexFormatInfo {
            format: wgpu::VertexFormat::Float32x2,
            size: 8,
            align: 4,
        },
        // R32_FLOAT
        41 => VertexFormatInfo {
            format: wgpu::VertexFormat::Float32,
            size: 4,
            align: 4,
        },
        // R8G8B8A8_UNORM
        28 => VertexFormatInfo {
            format: wgpu::VertexFormat::Unorm8x4,
            size: 4,
            align: 4,
        },
        // B8G8R8A8_UNORM
        //
        // WebGPU does not have a dedicated BGRA vertex format in wgpu 0.20, so we expose this as
        // `Unorm8x4` and rely on higher-level shader translation to swizzle channels when needed.
        87 => VertexFormatInfo {
            format: wgpu::VertexFormat::Unorm8x4,
            size: 4,
            align: 4,
        },
        // R16G16_FLOAT
        34 => VertexFormatInfo {
            format: wgpu::VertexFormat::Float16x2,
            size: 4,
            align: 4,
        },
        // R16G16B16A16_FLOAT
        10 => VertexFormatInfo {
            format: wgpu::VertexFormat::Float16x4,
            size: 8,
            align: 4,
        },
        // R32_UINT
        42 => VertexFormatInfo {
            format: wgpu::VertexFormat::Uint32,
            size: 4,
            align: 4,
        },
        // R16_UINT
        //
        // WebGPU does not support scalar 16-bit vertex formats; the closest representation is
        // `uint16x2` which consumes 4 bytes. This matches D3D's 4-byte alignment rules but requires
        // shader translation to only consume the `.x` component (the `.y` value comes from padding
        // / undefined bytes).
        57 => VertexFormatInfo {
            format: wgpu::VertexFormat::Uint16x2,
            size: 4,
            align: 4,
        },
        _ => return Err(InputLayoutError::UnsupportedDxgiFormat(dxgi_format)),
    })
}

fn build_signature_map(
    vs_signature: &[VsInputSignatureElement],
) -> HashMap<SignatureSemanticKey, u32> {
    let mut out = HashMap::with_capacity(vs_signature.len());
    for s in vs_signature {
        out.insert(
            SignatureSemanticKey {
                semantic_name_hash: s.semantic_name_hash,
                semantic_index: s.semantic_index,
            },
            s.shader_location,
        );
    }
    out
}

/// Map a D3D10/11 ILAY input layout to WebGPU vertex buffer layouts.
///
/// - Looks up each input element's shader location by `(semantic_name_hash, semantic_index)` in the
///   vertex shader input signature.
/// - Uses the signature's `shader_location` as WGSL `@location` / `wgpu::VertexAttribute::shader_location`.
/// - Resolves `D3D11_APPEND_ALIGNED_ELEMENT` offsets per input slot.
/// - Applies `slot_strides` to populate `wgpu::VertexBufferLayout::array_stride`.
///
/// Note: WebGPU does not support `instance_data_step_rate > 1`. For now this implementation clamps
/// the step rate to 1 (matching "P0" expectations in the task description). A future implementation
/// could emulate higher step rates by expanding instance data on the CPU.
pub fn map_layout_to_shader_locations(
    layout: &InputLayoutBinding<'_>,
    vs_signature: &[VsInputSignatureElement],
) -> Result<Vec<VertexBufferLayoutOwned>, InputLayoutError> {
    if layout.layout.elements.len() > MAX_WGPU_VERTEX_ATTRIBUTES as usize {
        return Err(InputLayoutError::TooManyVertexAttributes {
            count: layout.layout.elements.len() as u32,
            max: MAX_WGPU_VERTEX_ATTRIBUTES,
        });
    }

    let sig_map = build_signature_map(vs_signature);

    struct SlotState {
        next_offset: u32,
        required_stride: u32,
        step_mode: Option<wgpu::VertexStepMode>,
        attributes: Vec<wgpu::VertexAttribute>,
    }

    let mut slots: BTreeMap<u32, SlotState> = BTreeMap::new();
    let mut used_locations: HashMap<u32, ()> = HashMap::new();

    for elem in &layout.layout.elements {
        let key = SignatureSemanticKey {
            semantic_name_hash: elem.semantic_name_hash,
            semantic_index: elem.semantic_index,
        };
        let shader_location = *sig_map.get(&key).ok_or(InputLayoutError::MissingSemantic {
            semantic_name_hash: elem.semantic_name_hash,
            semantic_index: elem.semantic_index,
        })?;

        if used_locations.insert(shader_location, ()).is_some() {
            return Err(InputLayoutError::DuplicateShaderLocation {
                shader_location,
            });
        }

        let fmt = dxgi_format_to_vertex_format(elem.dxgi_format)?;
        let step_mode = match elem.input_slot_class {
            0 => wgpu::VertexStepMode::Vertex,
            1 => {
                if elem.instance_data_step_rate == 0 {
                    return Err(InputLayoutError::InvalidInstanceStepRate(
                        elem.instance_data_step_rate,
                    ));
                }
                // Clamp step_rate>1 to 1: WebGPU can't represent it directly.
                wgpu::VertexStepMode::Instance
            }
            other => return Err(InputLayoutError::UnsupportedInputSlotClass(other)),
        };

        let slot = slots.entry(elem.input_slot).or_insert_with(|| SlotState {
            next_offset: 0,
            required_stride: 0,
            step_mode: None,
            attributes: Vec::new(),
        });

        if let Some(prev_mode) = slot.step_mode {
            if prev_mode != step_mode {
                return Err(InputLayoutError::MixedStepModeInSlot {
                    slot: elem.input_slot,
                    first: prev_mode,
                    second: step_mode,
                });
            }
        } else {
            slot.step_mode = Some(step_mode);
        }

        let offset = if elem.aligned_byte_offset == D3D11_APPEND_ALIGNED_ELEMENT {
            align_up(slot.next_offset, fmt.align.max(1))
        } else {
            elem.aligned_byte_offset
        };

        if fmt.align > 1 && (offset % fmt.align) != 0 {
            return Err(InputLayoutError::MisalignedOffset {
                slot: elem.input_slot,
                offset,
                alignment: fmt.align,
            });
        }

        let end = offset
            .checked_add(fmt.size)
            .ok_or(InputLayoutError::OffsetOverflow {
                slot: elem.input_slot,
                offset,
                size: fmt.size,
            })?;
        slot.next_offset = end;
        slot.required_stride = slot.required_stride.max(end);

        slot.attributes.push(wgpu::VertexAttribute {
            shader_location,
            offset: offset as u64,
            format: fmt.format,
        });
    }

    // Build output layouts. Preserve D3D slot indices: the returned vector index corresponds to the
    // input slot. Unused slots in-between are emitted as empty layouts.
    let max_slot = slots.keys().copied().max().unwrap_or(0);
    let mut out: Vec<VertexBufferLayoutOwned> = Vec::new();
    if slots.is_empty() {
        return Ok(out);
    }
    if max_slot >= MAX_WGPU_VERTEX_BUFFERS {
        return Err(InputLayoutError::TooManyVertexBuffers {
            max_slot,
            max: MAX_WGPU_VERTEX_BUFFERS,
        });
    }

    out.reserve((max_slot + 1) as usize);
    for slot_index in 0..=max_slot {
        let Some(mut slot_state) = slots.remove(&slot_index) else {
            // Empty layout for an unused slot. `array_stride` is arbitrary, but must satisfy wgpu
            // validation (4-byte aligned).
            out.push(VertexBufferLayoutOwned {
                array_stride: 4,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: Vec::new(),
            });
            continue;
        };

        let stride = layout
            .slot_strides
            .get(slot_index as usize)
            .copied()
            .unwrap_or(0);
        if stride == 0 {
            return Err(InputLayoutError::MissingSlotStride { slot: slot_index });
        }
        if stride < slot_state.required_stride {
            return Err(InputLayoutError::StrideTooSmall {
                slot: slot_index,
                stride,
                required: slot_state.required_stride,
            });
        }

        // Sort attributes for deterministic pipeline keys and stable wgpu validation messages.
        slot_state
            .attributes
            .sort_by_key(|a| (a.shader_location, a.offset));

        out.push(VertexBufferLayoutOwned {
            array_stride: stride as u64,
            step_mode: slot_state.step_mode.unwrap_or(wgpu::VertexStepMode::Vertex),
            attributes: slot_state.attributes,
        });
    }

    Ok(out)
}

/// Like [`map_layout_to_shader_locations`], but compacts sparse D3D slot indices into a dense WebGPU
/// slot range.
///
/// Example: if a D3D input layout references slots 0 and 15, WebGPU cannot represent that directly
/// (it only allows up to 8 vertex buffers, and slot indices are dense). This function will emit
/// two vertex buffer layouts in WebGPU slots 0 and 1, and return a mapping `{0→0, 15→1}`.
pub fn map_layout_to_shader_locations_compact(
    layout: &InputLayoutBinding<'_>,
    vs_signature: &[VsInputSignatureElement],
) -> Result<MappedInputLayout, InputLayoutError> {
    if layout.layout.elements.len() > MAX_WGPU_VERTEX_ATTRIBUTES as usize {
        return Err(InputLayoutError::TooManyVertexAttributes {
            count: layout.layout.elements.len() as u32,
            max: MAX_WGPU_VERTEX_ATTRIBUTES,
        });
    }

    let sig_map = build_signature_map(vs_signature);

    struct SlotState {
        next_offset: u32,
        required_stride: u32,
        step_mode: Option<wgpu::VertexStepMode>,
        attributes: Vec<wgpu::VertexAttribute>,
    }

    let mut slots: BTreeMap<u32, SlotState> = BTreeMap::new();
    let mut used_locations: HashMap<u32, ()> = HashMap::new();

    for elem in &layout.layout.elements {
        let key = SignatureSemanticKey {
            semantic_name_hash: elem.semantic_name_hash,
            semantic_index: elem.semantic_index,
        };
        let shader_location = *sig_map.get(&key).ok_or(InputLayoutError::MissingSemantic {
            semantic_name_hash: elem.semantic_name_hash,
            semantic_index: elem.semantic_index,
        })?;

        if used_locations.insert(shader_location, ()).is_some() {
            return Err(InputLayoutError::DuplicateShaderLocation {
                shader_location,
            });
        }

        let fmt = dxgi_format_to_vertex_format(elem.dxgi_format)?;
        let step_mode = match elem.input_slot_class {
            0 => wgpu::VertexStepMode::Vertex,
            1 => {
                if elem.instance_data_step_rate == 0 {
                    return Err(InputLayoutError::InvalidInstanceStepRate(
                        elem.instance_data_step_rate,
                    ));
                }
                // Clamp step_rate>1 to 1: WebGPU can't represent it directly.
                wgpu::VertexStepMode::Instance
            }
            other => return Err(InputLayoutError::UnsupportedInputSlotClass(other)),
        };

        let slot = slots.entry(elem.input_slot).or_insert_with(|| SlotState {
            next_offset: 0,
            required_stride: 0,
            step_mode: None,
            attributes: Vec::new(),
        });

        if let Some(prev_mode) = slot.step_mode {
            if prev_mode != step_mode {
                return Err(InputLayoutError::MixedStepModeInSlot {
                    slot: elem.input_slot,
                    first: prev_mode,
                    second: step_mode,
                });
            }
        } else {
            slot.step_mode = Some(step_mode);
        }

        let offset = if elem.aligned_byte_offset == D3D11_APPEND_ALIGNED_ELEMENT {
            align_up(slot.next_offset, fmt.align.max(1))
        } else {
            elem.aligned_byte_offset
        };

        if fmt.align > 1 && (offset % fmt.align) != 0 {
            return Err(InputLayoutError::MisalignedOffset {
                slot: elem.input_slot,
                offset,
                alignment: fmt.align,
            });
        }

        let end = offset
            .checked_add(fmt.size)
            .ok_or(InputLayoutError::OffsetOverflow {
                slot: elem.input_slot,
                offset,
                size: fmt.size,
            })?;
        slot.next_offset = end;
        slot.required_stride = slot.required_stride.max(end);

        slot.attributes.push(wgpu::VertexAttribute {
            shader_location,
            offset: offset as u64,
            format: fmt.format,
        });
    }

    if slots.len() > MAX_WGPU_VERTEX_BUFFERS as usize {
        return Err(InputLayoutError::TooManyUsedVertexBuffers {
            count: slots.len() as u32,
            max: MAX_WGPU_VERTEX_BUFFERS,
        });
    }

    let mut buffers = Vec::with_capacity(slots.len());
    let mut slot_map = BTreeMap::new();

    for (wgpu_slot, (d3d_slot, mut slot_state)) in slots.into_iter().enumerate() {
        slot_map.insert(d3d_slot, wgpu_slot as u32);

        let stride = layout
            .slot_strides
            .get(d3d_slot as usize)
            .copied()
            .unwrap_or(0);
        if stride == 0 {
            return Err(InputLayoutError::MissingSlotStride { slot: d3d_slot });
        }
        if stride < slot_state.required_stride {
            return Err(InputLayoutError::StrideTooSmall {
                slot: d3d_slot,
                stride,
                required: slot_state.required_stride,
            });
        }

        slot_state
            .attributes
            .sort_by_key(|a| (a.shader_location, a.offset));

        buffers.push(VertexBufferLayoutOwned {
            array_stride: stride as u64,
            step_mode: slot_state.step_mode.unwrap_or(wgpu::VertexStepMode::Vertex),
            attributes: slot_state.attributes,
        });
    }

    Ok(MappedInputLayout {
        buffers,
        d3d_slot_to_wgpu_slot: slot_map,
    })
}

/// Compute a 32-bit FNV-1a hash (used for semantic name hashing in the ILAY protocol).
///
/// Note: D3D semantic matching is case-insensitive; the AeroGPU ILAY protocol hashes the
/// semantic name after canonicalizing it to ASCII uppercase.
pub fn fnv1a_32(bytes: &[u8]) -> u32 {
    const OFFSET: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn parses_ilay_blob() {
        let mut blob = Vec::new();
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut blob, 1); // element_count
        push_u32(&mut blob, 0); // reserved0

        // One element: POSITION0, R32G32B32_FLOAT, slot 0, offset 0.
        push_u32(&mut blob, 0xDEAD_BEEF); // semantic hash
        push_u32(&mut blob, 0); // semantic index
        push_u32(&mut blob, 6); // DXGI_FORMAT_R32G32B32_FLOAT
        push_u32(&mut blob, 0); // input_slot
        push_u32(&mut blob, 0); // offset
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let parsed = InputLayoutDesc::parse(&blob).expect("parse failed");
        assert_eq!(
            parsed.header,
            InputLayoutBlobHeader {
                magic: AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
                version: AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
                element_count: 1
            }
        );
        assert_eq!(parsed.elements.len(), 1);
        assert_eq!(
            parsed.elements[0],
            InputLayoutElementDxgi {
                semantic_name_hash: 0xDEAD_BEEF,
                semantic_index: 0,
                dxgi_format: 6,
                input_slot: 0,
                aligned_byte_offset: 0,
                input_slot_class: 0,
                instance_data_step_rate: 0
            }
        );
    }

    #[test]
    fn parses_ilay_blob_with_trailing_bytes() {
        let mut blob = Vec::new();
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut blob, 1); // element_count
        push_u32(&mut blob, 0); // reserved0

        // One element.
        push_u32(&mut blob, 0xDEAD_BEEF); // semantic hash
        push_u32(&mut blob, 0); // semantic index
        push_u32(&mut blob, 6); // DXGI_FORMAT_R32G32B32_FLOAT
        push_u32(&mut blob, 0); // input_slot
        push_u32(&mut blob, 0); // offset
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        // Extra extension bytes (future-proofing).
        blob.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        blob.extend_from_slice(&0x9abc_def0u32.to_le_bytes());

        let parsed = InputLayoutDesc::parse(&blob).expect("parse failed");
        assert_eq!(parsed.elements.len(), 1);
        assert_eq!(parsed.elements[0].semantic_name_hash, 0xDEAD_BEEF);
    }

    #[test]
    fn maps_semantics_and_append_offsets() {
        let pos_hash = fnv1a_32(b"POSITION");
        let uv_hash = fnv1a_32(b"TEXCOORD");

        let mut blob = Vec::new();
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut blob, 2); // element_count
        push_u32(&mut blob, 0); // reserved0

        // POSITION0: float3 @ offset 0
        push_u32(&mut blob, pos_hash);
        push_u32(&mut blob, 0);
        push_u32(&mut blob, 6); // R32G32B32_FLOAT
        push_u32(&mut blob, 0); // slot 0
        push_u32(&mut blob, 0); // explicit offset 0
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        // TEXCOORD0: float2 @ append
        push_u32(&mut blob, uv_hash);
        push_u32(&mut blob, 0);
        push_u32(&mut blob, 16); // R32G32_FLOAT
        push_u32(&mut blob, 0); // slot 0
        push_u32(&mut blob, D3D11_APPEND_ALIGNED_ELEMENT);
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let layout = InputLayoutDesc::parse(&blob).expect("parse failed");

        let signature = [
            VsInputSignatureElement {
                semantic_name_hash: pos_hash,
                semantic_index: 0,
                input_register: 0,
                mask: 0xF,
                shader_location: 0,
            },
            VsInputSignatureElement {
                semantic_name_hash: uv_hash,
                semantic_index: 0,
                input_register: 1,
                mask: 0xF,
                shader_location: 1,
            },
        ];

        let strides = [20u32]; // float3 (12) + float2 (8)
        let binding = InputLayoutBinding::new(&layout, &strides);
        let mapped = map_layout_to_shader_locations(&binding, &signature).expect("mapping failed");

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].array_stride, 20);
        assert_eq!(mapped[0].step_mode, wgpu::VertexStepMode::Vertex);

        assert_eq!(mapped[0].attributes.len(), 2);
        assert_eq!(mapped[0].attributes[0].shader_location, 0);
        assert_eq!(mapped[0].attributes[0].offset, 0);
        assert_eq!(
            mapped[0].attributes[0].format,
            wgpu::VertexFormat::Float32x3
        );

        assert_eq!(mapped[0].attributes[1].shader_location, 1);
        assert_eq!(mapped[0].attributes[1].offset, 12);
        assert_eq!(
            mapped[0].attributes[1].format,
            wgpu::VertexFormat::Float32x2
        );
    }

    #[test]
    fn maps_packed_signature_registers_to_distinct_shader_locations() {
        // D3D signatures can pack multiple semantics into one input register. WebGPU vertex
        // attributes require unique shader locations, so we allow the signature to provide a
        // `shader_location` that differs from the packed `input_register`.
        let pos_hash = fnv1a_32(b"POSITION");
        let uv_hash = fnv1a_32(b"TEXCOORD");

        let mut blob = Vec::new();
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut blob, 2); // element_count
        push_u32(&mut blob, 0); // reserved0

        // POSITION0: float2 @ offset 0.
        push_u32(&mut blob, pos_hash);
        push_u32(&mut blob, 0);
        push_u32(&mut blob, 16); // R32G32_FLOAT
        push_u32(&mut blob, 0); // slot 0
        push_u32(&mut blob, 0); // offset 0
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        // TEXCOORD0: float2 @ offset 8.
        push_u32(&mut blob, uv_hash);
        push_u32(&mut blob, 0);
        push_u32(&mut blob, 16); // R32G32_FLOAT
        push_u32(&mut blob, 0); // slot 0
        push_u32(&mut blob, 8); // offset 8
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let layout = InputLayoutDesc::parse(&blob).expect("parse failed");
        let signature = [
            VsInputSignatureElement {
                semantic_name_hash: pos_hash,
                semantic_index: 0,
                input_register: 0,
                mask: 0b0011,
                shader_location: 0,
            },
            VsInputSignatureElement {
                semantic_name_hash: uv_hash,
                semantic_index: 0,
                input_register: 0,
                mask: 0b1100,
                shader_location: 1,
            },
        ];

        let strides = [16u32];
        let binding = InputLayoutBinding::new(&layout, &strides);
        let mapped = map_layout_to_shader_locations(&binding, &signature).expect("mapping failed");

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].attributes.len(), 2);
        assert_eq!(mapped[0].attributes[0].shader_location, 0);
        assert_eq!(mapped[0].attributes[1].shader_location, 1);
    }

    #[test]
    fn compacts_sparse_input_slots() {
        let pos_hash = fnv1a_32(b"POSITION");
        let uv_hash = fnv1a_32(b"TEXCOORD");

        let mut blob = Vec::new();
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut blob, 2); // element_count
        push_u32(&mut blob, 0); // reserved0

        // TEXCOORD0 in slot 0.
        push_u32(&mut blob, uv_hash);
        push_u32(&mut blob, 0);
        push_u32(&mut blob, 16); // R32G32_FLOAT
        push_u32(&mut blob, 0); // slot 0
        push_u32(&mut blob, 0); // offset 0
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        // POSITION0 in slot 31.
        push_u32(&mut blob, pos_hash);
        push_u32(&mut blob, 0);
        push_u32(&mut blob, 6); // R32G32B32_FLOAT
        push_u32(&mut blob, 31); // slot 31
        push_u32(&mut blob, 0); // offset 0
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let layout = InputLayoutDesc::parse(&blob).expect("parse failed");
        let signature = [
            VsInputSignatureElement {
                semantic_name_hash: uv_hash,
                semantic_index: 0,
                input_register: 0,
                mask: 0xF,
                shader_location: 0,
            },
            VsInputSignatureElement {
                semantic_name_hash: pos_hash,
                semantic_index: 0,
                input_register: 1,
                mask: 0xF,
                shader_location: 1,
            },
        ];

        let mut strides = vec![0u32; 32];
        strides[0] = 8;
        strides[31] = 12;
        let binding = InputLayoutBinding::new(&layout, &strides);

        // Preserve-slots mapping fails because WebGPU can't address slot 31 directly.
        assert!(matches!(
            map_layout_to_shader_locations(&binding, &signature),
            Err(InputLayoutError::TooManyVertexBuffers { max_slot: 31, .. })
        ));

        // Compact mapping succeeds and provides a D3D->WebGPU slot map.
        let mapped =
            map_layout_to_shader_locations_compact(&binding, &signature).expect("compact mapping");
        assert_eq!(mapped.buffers.len(), 2);
        assert_eq!(mapped.d3d_slot_to_wgpu_slot.get(&0), Some(&0));
        assert_eq!(mapped.d3d_slot_to_wgpu_slot.get(&31), Some(&1));

        assert_eq!(mapped.buffers[0].array_stride, 8);
        assert_eq!(mapped.buffers[0].attributes.len(), 1);
        assert_eq!(mapped.buffers[0].attributes[0].shader_location, 0);
        assert_eq!(
            mapped.buffers[0].attributes[0].format,
            wgpu::VertexFormat::Float32x2
        );

        assert_eq!(mapped.buffers[1].array_stride, 12);
        assert_eq!(mapped.buffers[1].attributes.len(), 1);
        assert_eq!(mapped.buffers[1].attributes[0].shader_location, 1);
        assert_eq!(
            mapped.buffers[1].attributes[0].format,
            wgpu::VertexFormat::Float32x3
        );
    }

    #[test]
    fn maps_semantic_indices_with_same_name() {
        let tex_hash = fnv1a_32(b"TEXCOORD");
        let mut blob = Vec::new();
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut blob, 2); // element_count
        push_u32(&mut blob, 0); // reserved0

        // TEXCOORD0: float2 @ offset 0
        push_u32(&mut blob, tex_hash);
        push_u32(&mut blob, 0); // semantic index
        push_u32(&mut blob, 16); // R32G32_FLOAT
        push_u32(&mut blob, 0); // slot 0
        push_u32(&mut blob, 0); // offset 0
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        // TEXCOORD1: float2 @ offset 8
        push_u32(&mut blob, tex_hash);
        push_u32(&mut blob, 1); // semantic index
        push_u32(&mut blob, 16); // R32G32_FLOAT
        push_u32(&mut blob, 0); // slot 0
        push_u32(&mut blob, 8); // offset 8
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let layout = InputLayoutDesc::parse(&blob).expect("parse failed");

        // Use "swapped" registers so we can verify mapping is keyed by semantic_index (not just
        // semantic name or element order).
        let signature = [
            VsInputSignatureElement {
                semantic_name_hash: tex_hash,
                semantic_index: 0,
                input_register: 1,
                mask: 0xF,
                shader_location: 1,
            },
            VsInputSignatureElement {
                semantic_name_hash: tex_hash,
                semantic_index: 1,
                input_register: 0,
                mask: 0xF,
                shader_location: 0,
            },
        ];

        let strides = [16u32];
        let binding = InputLayoutBinding::new(&layout, &strides);
        let mapped =
            map_layout_to_shader_locations_compact(&binding, &signature).expect("compact mapping");

        assert_eq!(mapped.buffers.len(), 1);
        assert_eq!(mapped.d3d_slot_to_wgpu_slot.get(&0), Some(&0));
        assert_eq!(mapped.buffers[0].array_stride, 16);

        let attrs = &mapped.buffers[0].attributes;
        assert_eq!(attrs.len(), 2);
        let mut loc_at_0 = None;
        let mut loc_at_8 = None;
        for a in attrs {
            match a.offset {
                0 => loc_at_0 = Some(a.shader_location),
                8 => loc_at_8 = Some(a.shader_location),
                _ => {}
            }
        }
        assert_eq!(loc_at_0, Some(1));
        assert_eq!(loc_at_8, Some(0));
    }

    #[test]
    fn rejects_layout_with_too_many_vertex_attributes() {
        let element_count = MAX_WGPU_VERTEX_ATTRIBUTES + 1;

        let mut blob = Vec::new();
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut blob, element_count);
        push_u32(&mut blob, 0); // reserved0

        for i in 0..element_count {
            push_u32(&mut blob, i); // semantic hash (arbitrary)
            push_u32(&mut blob, 0); // semantic index
            push_u32(&mut blob, 41); // DXGI_FORMAT_R32_FLOAT
            push_u32(&mut blob, 0); // input_slot
            push_u32(&mut blob, 0); // offset
            push_u32(&mut blob, 0); // per-vertex
            push_u32(&mut blob, 0); // step rate
        }

        let layout = InputLayoutDesc::parse(&blob).expect("parse failed");
        let binding = InputLayoutBinding::new(&layout, &[]);
        assert!(matches!(
            map_layout_to_shader_locations_compact(&binding, &[]),
            Err(InputLayoutError::TooManyVertexAttributes { count, max })
                if count == element_count && max == MAX_WGPU_VERTEX_ATTRIBUTES
        ));
    }

    #[test]
    fn rejects_layout_that_needs_too_many_vertex_buffers_after_compaction() {
        let element_count = MAX_WGPU_VERTEX_BUFFERS + 1;

        let mut blob = Vec::new();
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut blob, element_count);
        push_u32(&mut blob, 0); // reserved0

        let mut signature = Vec::new();
        let mut strides = vec![0u32; element_count as usize];
        for i in 0..element_count {
            push_u32(&mut blob, i); // semantic hash (arbitrary)
            push_u32(&mut blob, 0); // semantic index
            push_u32(&mut blob, 41); // DXGI_FORMAT_R32_FLOAT
            push_u32(&mut blob, i); // input_slot
            push_u32(&mut blob, 0); // offset
            push_u32(&mut blob, 0); // per-vertex
            push_u32(&mut blob, 0); // step rate

            signature.push(VsInputSignatureElement {
                semantic_name_hash: i,
                semantic_index: 0,
                input_register: i,
                mask: 0xF,
                shader_location: i,
            });
            strides[i as usize] = 4;
        }

        let layout = InputLayoutDesc::parse(&blob).expect("parse failed");
        let binding = InputLayoutBinding::new(&layout, &strides);
        assert!(matches!(
            map_layout_to_shader_locations_compact(&binding, &signature),
            Err(InputLayoutError::TooManyUsedVertexBuffers { count, max })
                if count == element_count && max == MAX_WGPU_VERTEX_BUFFERS
        ));
    }
}
