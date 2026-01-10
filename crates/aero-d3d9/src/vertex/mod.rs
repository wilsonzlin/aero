//! Vertex input translation (FVF / vertex declarations) for D3D9.

mod declaration;
mod format_map;
mod fvf;
mod instancing;
mod location_map;
mod wgsl;

pub use declaration::{
    D3dVertexElement9, DeclMethod, DeclType, DeclUsage, VertexDeclaration, VertexElement,
    VertexInputError,
};
pub use format_map::{ElementConversion, ElementFormat, WebGpuVertexCaps};
pub use fvf::{Fvf, FvfDecodeError, FvfLayout};
pub use instancing::{
    expand_instance_data, InstanceDataExpandError, StreamSourceFreq, StreamSourceFreqParseError,
    StreamStep, StreamStepState, StreamsFreqState,
};
pub use location_map::{
    FixedFunctionLocationMap, LocationMapError, StandardLocationMap, VertexLocationMap,
};
pub use wgsl::{wgsl_vertex_input_fields, WgslTypeInfo, WgslVertexInputField};

use crate::vertex::format_map::map_element_format;
use std::collections::BTreeMap;
use wgpu::{BufferAddress, VertexAttribute, VertexFormat, VertexStepMode};

/// Owned `wgpu::VertexBufferLayout`.
///
/// `wgpu::VertexBufferLayout<'a>` borrows the attribute slice which makes it awkward to
/// store in cache keys and pipeline descriptors. We store an owned variant and provide a helper
/// to borrow it.
#[derive(Debug, Clone, PartialEq)]
pub struct VertexBufferLayoutOwned {
    pub array_stride: BufferAddress,
    pub step_mode: VertexStepMode,
    pub attributes: Vec<VertexAttribute>,
}

impl VertexBufferLayoutOwned {
    /// Borrow the attributes slice; useful when building a `wgpu` pipeline descriptor.
    pub fn attributes(&self) -> &[VertexAttribute] {
        &self.attributes
    }
}

/// Translation output for a D3D9 vertex input definition.
#[derive(Debug, Clone, PartialEq)]
pub struct TranslatedVertexInput {
    /// WebGPU vertex buffer layouts (one per bound buffer slot).
    pub buffers: Vec<VertexBufferLayoutOwned>,

    /// Mapping from D3D stream index → WebGPU buffer slot.
    pub stream_to_buffer_slot: BTreeMap<u8, u32>,

    /// Derived instancing state.
    pub instancing: StreamStepState,

    /// CPU-side conversion plans per D3D stream (if needed).
    pub conversions: BTreeMap<u8, StreamConversionPlan>,
}

/// Conversion plan for a single D3D stream when the declaration contains formats that can't be
/// represented directly in WebGPU (or require unavailable features, e.g. f16 vertex attributes).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamConversionPlan {
    pub src_stride: u32,
    pub dst_stride: u32,
    pub elements: Vec<ElementConversionPlan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElementConversionPlan {
    pub src_offset: u32,
    pub dst_offset: u32,
    pub src_type: DeclType,
    pub dst_format: VertexFormat,
    pub conversion: ElementConversion,
}

impl StreamConversionPlan {
    /// Convert `vertex_count` vertices from `src` into a tightly packed stream described by this
    /// plan.
    pub fn convert_vertices(&self, src: &[u8], vertex_count: usize) -> Result<Vec<u8>, VertexInputError> {
        let src_stride = self.src_stride as usize;
        let dst_stride = self.dst_stride as usize;
        if src.len() < src_stride * vertex_count {
            return Err(VertexInputError::VertexDataTooSmall {
                expected: src_stride * vertex_count,
                actual: src.len(),
            });
        }

        let mut dst = vec![0u8; dst_stride * vertex_count];
        for v in 0..vertex_count {
            let src_base = v * src_stride;
            let dst_base = v * dst_stride;
            for e in &self.elements {
                let src_off = src_base + e.src_offset as usize;
                let dst_off = dst_base + e.dst_offset as usize;
                format_map::convert_element(e, &src[src_off..], &mut dst[dst_off..])?;
            }
        }

        Ok(dst)
    }
}

/// Translate a D3D9 vertex declaration into WebGPU layouts.
///
/// * `stream_strides` is the stride set via `SetStreamSource` for each D3D stream.
/// * `stream_freq` is the `SetStreamSourceFreq` state for each D3D stream.
/// * `location_map` decides how usages map to WGSL `@location`.
pub fn translate_vertex_declaration(
    decl: &VertexDeclaration,
    stream_strides: &BTreeMap<u8, u32>,
    stream_freq: &StreamsFreqState,
    caps: WebGpuVertexCaps,
    location_map: &dyn VertexLocationMap,
) -> Result<TranslatedVertexInput, VertexInputError> {
    // WebGPU minimum limits (and D3D9 compatibility expectations).
    const MAX_VERTEX_BUFFERS: usize = 8;
    const MAX_VERTEX_ATTRIBUTES: usize = 16;

    if decl.elements.len() > MAX_VERTEX_ATTRIBUTES {
        return Err(VertexInputError::TooManyVertexAttributes {
            count: decl.elements.len(),
            max: MAX_VERTEX_ATTRIBUTES,
        });
    }

    let mut used_streams: Vec<u8> = decl
        .elements
        .iter()
        .map(|e| e.stream)
        .collect();
    used_streams.sort_unstable();
    used_streams.dedup();

    if used_streams.len() > MAX_VERTEX_BUFFERS {
        return Err(VertexInputError::TooManyVertexBuffers {
            count: used_streams.len(),
            max: MAX_VERTEX_BUFFERS,
        });
    }

    // Ensure no two elements map to the same shader location.
    let mut seen_locations = BTreeMap::<u32, (DeclUsage, u8)>::new();
    for e in &decl.elements {
        let loc = location_map.location_for(e.usage, e.usage_index)?;
        if let Some((prev_usage, prev_index)) = seen_locations.insert(loc, (e.usage, e.usage_index))
        {
            return Err(VertexInputError::DuplicateShaderLocation {
                location: loc,
                first_usage: prev_usage,
                first_usage_index: prev_index,
                second_usage: e.usage,
                second_usage_index: e.usage_index,
            });
        }
    }

    let mut stream_to_buffer_slot = BTreeMap::<u8, u32>::new();
    for (slot, stream) in used_streams.iter().copied().enumerate() {
        stream_to_buffer_slot.insert(stream, slot as u32);
    }

    // Group elements by D3D stream.
    let mut by_stream: BTreeMap<u8, Vec<&VertexElement>> = BTreeMap::new();
    for e in &decl.elements {
        by_stream.entry(e.stream).or_default().push(e);
    }
    for elems in by_stream.values_mut() {
        elems.sort_by_key(|e| e.offset);
    }

    let instancing = stream_freq.compute_stream_step()?;

    let mut buffers = Vec::new();
    let mut conversions = BTreeMap::<u8, StreamConversionPlan>::new();

    for stream in used_streams {
        let elems = by_stream.get(&stream).expect("stream must exist");
        let Some(&stride) = stream_strides.get(&stream) else {
            return Err(VertexInputError::MissingStreamStride { stream });
        };
        if stride == 0 {
            return Err(VertexInputError::ZeroStreamStride { stream });
        }

        // Validate D3D stride covers the declared elements.
        let required = elems
            .iter()
            .map(|e| e.offset as u32 + e.ty.byte_size())
            .max()
            .unwrap_or(0);
        if stride < required {
            return Err(VertexInputError::StrideTooSmall {
                stream,
                stride,
                required,
            });
        }

        // Determine if this stream requires conversion.
        let mut needs_conversion = false;
        for e in elems {
            let mapped = map_element_format(e.ty, caps)?;
            if mapped.conversion != ElementConversion::None {
                needs_conversion = true;
                break;
            }
        }

        let step_mode = match instancing.stream_step(stream) {
            StreamStep::Instance { .. } => VertexStepMode::Instance,
            StreamStep::Vertex => VertexStepMode::Vertex,
        };

        if !needs_conversion {
            let mut attributes = Vec::new();
            for e in elems {
                let mapped = map_element_format(e.ty, caps)?;
                let location = location_map.location_for(e.usage, e.usage_index)?;
                attributes.push(VertexAttribute {
                    format: mapped.format,
                    offset: e.offset as BufferAddress,
                    shader_location: location,
                });
            }
            buffers.push(VertexBufferLayoutOwned {
                array_stride: stride as BufferAddress,
                step_mode,
                attributes,
            });
            continue;
        }

        // Conversion: pack a new stream buffer with WebGPU-friendly formats.
        let mut dst_offset = 0u32;
        let mut attributes = Vec::new();
        let mut plans = Vec::new();
        for e in elems {
            // Keep attributes 4-byte aligned; simplifies CPU conversion and matches typical
            // GPU alignment constraints.
            dst_offset = align_up(dst_offset, 4);

            let mapped = map_element_format(e.ty, caps)?;
            let location = location_map.location_for(e.usage, e.usage_index)?;
            attributes.push(VertexAttribute {
                format: mapped.format,
                offset: dst_offset as BufferAddress,
                shader_location: location,
            });
            plans.push(ElementConversionPlan {
                src_offset: e.offset as u32,
                dst_offset,
                src_type: e.ty,
                dst_format: mapped.format,
                conversion: mapped.conversion,
            });
            dst_offset += mapped.byte_size;
        }
        let dst_stride = align_up(dst_offset, 4);
        buffers.push(VertexBufferLayoutOwned {
            array_stride: dst_stride as BufferAddress,
            step_mode,
            attributes,
        });
        conversions.insert(
            stream,
            StreamConversionPlan {
                src_stride: stride,
                dst_stride,
                elements: plans,
            },
        );
    }

    Ok(TranslatedVertexInput {
        buffers,
        stream_to_buffer_slot,
        instancing,
        conversions,
    })
}

fn align_up(v: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (v + (align - 1)) & !(align - 1)
}
