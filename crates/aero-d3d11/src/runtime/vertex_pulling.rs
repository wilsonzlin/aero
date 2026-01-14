//! GPU-side vertex pulling helpers for executing vertex (and later hull/domain) stages as compute.
//!
//! When emulating GS/HS/DS on platforms without native support we need to run the vertex stage as a
//! compute shader. Compute entry points cannot read attributes from WebGPU's vertex input
//! interface, so we "pull" them manually from the D3D11 input-assembler vertex buffers.
//!
//! This module defines:
//! - A canonical bind-group layout for IA vertex buffers + a small uniform with per-slot stride and
//!   base offsets.
//! - WGSL codegen helpers that emit `load_attr_*` functions using `u32` loads + `bitcast` to avoid
//!   alignment traps.

use std::collections::{BTreeMap, HashMap};

use crate::binding_model::{BINDING_BASE_INTERNAL, BIND_GROUP_INTERNAL_EMULATION};
use crate::input_layout::{
    dxgi_format_info, InputLayoutBinding, InputLayoutError, SignatureSemanticKey,
    VsInputSignatureElement, D3D11_APPEND_ALIGNED_ELEMENT, MAX_WGPU_VERTEX_ATTRIBUTES,
    MAX_WGPU_VERTEX_BUFFERS,
};

/// Reserved bind-group index for IA vertex pulling resources.
///
/// Group indices `0..=2` are used by the D3D binding model (`binding_model.rs`) for VS/PS/CS
/// resources. WebGPU guarantees `maxBindGroups >= 4`, so AeroGPU uses `@group(3)` for both:
/// - Extended D3D11 stage (GS/HS/DS) resources
/// - Internal emulation helpers like vertex pulling
///
/// Internal bindings within this group must use `@binding >= BINDING_BASE_INTERNAL` to avoid
/// colliding with the D3D11 register-space binding ranges.
pub const VERTEX_PULLING_GROUP: u32 = BIND_GROUP_INTERNAL_EMULATION;

/// First `@binding` number reserved for vertex pulling + compute-expansion internal resources
/// within [`VERTEX_PULLING_GROUP`].
///
/// Using [`BINDING_BASE_INTERNAL`] keeps internal emulation bindings disjoint from the D3D11
/// register-space ranges (`b#`/`t#`/`s#`/`u#`), and makes it safe to colocate vertex pulling with
/// other internal helpers if that becomes necessary.
///
/// This is anchored on [`crate::binding_model::BINDING_BASE_INTERNAL`] so all internal bindings are
/// guaranteed to stay disjoint from the D3D11 register-space ranges (`b#`/`t#`/`s#`/`u#`).
pub const VERTEX_PULLING_BINDING_BASE: u32 = BINDING_BASE_INTERNAL;

/// Base `@binding` number for pulled IA vertex buffers.
///
/// Vertex buffers are bound at `VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + slot`.
pub const VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE: u32 = VERTEX_PULLING_BINDING_BASE + 1;

/// `@binding` number for the vertex pulling uniform buffer inside [`VERTEX_PULLING_GROUP`].
///
/// This lives in the reserved internal binding range.
pub const VERTEX_PULLING_UNIFORM_BINDING: u32 = VERTEX_PULLING_BINDING_BASE;

/// Per-slot vertex buffer dynamic state needed for address calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexPullingSlot {
    pub base_offset_bytes: u32,
    pub stride_bytes: u32,
}

/// Draw parameters consumed by vertex pulling compute shaders.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VertexPullingDrawParams {
    pub first_vertex: u32,
    pub first_instance: u32,
    /// For indexed draws, this is the `base_vertex` parameter from `DrawIndexed`.
    ///
    /// For non-indexed draws it should be zero.
    pub base_vertex: i32,
    /// For indexed draws, this is the first index in the index buffer (`first_index`).
    ///
    /// For non-indexed draws it should be zero.
    pub first_index: u32,
}

/// A single IA attribute load description derived from an [`InputLayoutDesc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexPullingAttribute {
    /// WGSL `@location` assigned by signature-driven translation.
    pub shader_location: u32,
    /// Compact vertex buffer slot index used by the pulling bind group (0..N).
    pub pulling_slot: u32,
    /// Offset of this element from the start of the vertex struct (bytes).
    pub offset_bytes: u32,
    /// DXGI format metadata used by codegen (component type/lanes).
    pub format: crate::input_layout::DxgiFormatInfo,
    /// 0 = per-vertex, 1 = per-instance.
    pub step_mode: wgpu::VertexStepMode,
    /// D3D11 instance-data step rate (valid only for [`wgpu::VertexStepMode::Instance`]).
    pub instance_step_rate: u32,
}

/// A compacted view of an ILAY input layout suitable for compute-side vertex pulling.
#[derive(Debug, Clone)]
pub struct VertexPullingLayout {
    /// D3D11 input slot → compact pulling slot.
    pub d3d_slot_to_pulling_slot: BTreeMap<u32, u32>,
    /// Pulling slot → D3D11 input slot.
    pub pulling_slot_to_d3d_slot: Vec<u32>,
    pub attributes: Vec<VertexPullingAttribute>,
}

fn align_up(v: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (v + (align - 1)) & !(align - 1)
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

impl VertexPullingLayout {
    /// Build a pulling layout from ILAY + bound IA strides.
    ///
    /// This mirrors [`crate::input_layout::map_layout_to_shader_locations_compact`] but retains
    /// DXGI format metadata and per-element offsets/step rates for code generation.
    pub fn new(
        binding: &InputLayoutBinding<'_>,
        vs_signature: &[VsInputSignatureElement],
    ) -> Result<Self, InputLayoutError> {
        if binding.layout.elements.len() > MAX_WGPU_VERTEX_ATTRIBUTES as usize {
            return Err(InputLayoutError::TooManyVertexAttributes {
                count: binding.layout.elements.len() as u32,
                max: MAX_WGPU_VERTEX_ATTRIBUTES,
            });
        }

        let sig_map = build_signature_map(vs_signature);

        #[derive(Default)]
        struct SlotState {
            next_offset: u32,
            required_stride: u32,
        }

        let mut slot_state: BTreeMap<u32, SlotState> = BTreeMap::new();
        let mut slot_step_mode: HashMap<u32, wgpu::VertexStepMode> = HashMap::new();
        let mut used_locations: HashMap<u32, ()> = HashMap::new();

        #[derive(Debug, Clone)]
        struct TempElem {
            shader_location: u32,
            d3d_slot: u32,
            offset_bytes: u32,
            format: crate::input_layout::DxgiFormatInfo,
            step_mode: wgpu::VertexStepMode,
            step_rate: u32,
        }

        let mut temp_elems: Vec<TempElem> = Vec::with_capacity(binding.layout.elements.len());

        for elem in &binding.layout.elements {
            let key = SignatureSemanticKey {
                semantic_name_hash: elem.semantic_name_hash,
                semantic_index: elem.semantic_index,
            };
            let shader_location = *sig_map.get(&key).ok_or(InputLayoutError::MissingSemantic {
                semantic_name_hash: elem.semantic_name_hash,
                semantic_index: elem.semantic_index,
            })?;
            if used_locations.insert(shader_location, ()).is_some() {
                return Err(InputLayoutError::DuplicateShaderLocation { shader_location });
            }

            let fmt = dxgi_format_info(elem.dxgi_format)?;

            let (step_mode, step_rate) = match elem.input_slot_class {
                0 => (wgpu::VertexStepMode::Vertex, 0),
                1 => {
                    if elem.instance_data_step_rate == 0 {
                        return Err(InputLayoutError::InvalidInstanceStepRate(
                            elem.instance_data_step_rate,
                        ));
                    }
                    (wgpu::VertexStepMode::Instance, elem.instance_data_step_rate)
                }
                other => return Err(InputLayoutError::UnsupportedInputSlotClass(other)),
            };

            if let Some(prev) = slot_step_mode.insert(elem.input_slot, step_mode) {
                if prev != step_mode {
                    return Err(InputLayoutError::MixedStepModeInSlot {
                        slot: elem.input_slot,
                        first: prev,
                        second: step_mode,
                    });
                }
            }

            let slot = slot_state.entry(elem.input_slot).or_default();
            let offset = if elem.aligned_byte_offset == D3D11_APPEND_ALIGNED_ELEMENT {
                align_up(slot.next_offset, fmt.align_bytes.max(1))
            } else {
                elem.aligned_byte_offset
            };

            if fmt.align_bytes > 1 && (offset % fmt.align_bytes) != 0 {
                return Err(InputLayoutError::MisalignedOffset {
                    slot: elem.input_slot,
                    offset,
                    alignment: fmt.align_bytes,
                });
            }

            let end =
                offset
                    .checked_add(fmt.size_bytes)
                    .ok_or(InputLayoutError::OffsetOverflow {
                        slot: elem.input_slot,
                        offset,
                        size: fmt.size_bytes,
                    })?;
            slot.next_offset = end;
            slot.required_stride = slot.required_stride.max(end);

            temp_elems.push(TempElem {
                shader_location,
                d3d_slot: elem.input_slot,
                offset_bytes: offset,
                format: fmt,
                step_mode,
                step_rate,
            });
        }

        // Validate vertex buffer count against the baseline WebGPU limit. Compute shaders share the
        // same minimum-per-stage storage buffer limits, so keeping this aligned avoids obscure
        // adapter-specific failures later.
        if slot_state.len() > MAX_WGPU_VERTEX_BUFFERS as usize {
            return Err(InputLayoutError::TooManyUsedVertexBuffers {
                count: slot_state.len() as u32,
                max: MAX_WGPU_VERTEX_BUFFERS,
            });
        }

        // Validate caller-provided strides.
        for (&d3d_slot, state) in &slot_state {
            let stride = binding
                .slot_strides
                .get(d3d_slot as usize)
                .copied()
                .unwrap_or(0);
            if stride == 0 {
                return Err(InputLayoutError::MissingSlotStride { slot: d3d_slot });
            }
            if stride < state.required_stride {
                return Err(InputLayoutError::StrideTooSmall {
                    slot: d3d_slot,
                    stride,
                    required: state.required_stride,
                });
            }
        }

        // Compact D3D slots into dense pulling slots (sorted by D3D slot index for determinism).
        let mut d3d_slot_to_pulling_slot: BTreeMap<u32, u32> = BTreeMap::new();
        let mut pulling_slot_to_d3d_slot: Vec<u32> = Vec::with_capacity(slot_state.len());
        for (pulling_slot, d3d_slot) in slot_state.keys().copied().enumerate() {
            d3d_slot_to_pulling_slot.insert(d3d_slot, pulling_slot as u32);
            pulling_slot_to_d3d_slot.push(d3d_slot);
        }

        let mut attributes: Vec<VertexPullingAttribute> = temp_elems
            .into_iter()
            .map(|e| VertexPullingAttribute {
                shader_location: e.shader_location,
                pulling_slot: *d3d_slot_to_pulling_slot.get(&e.d3d_slot).unwrap(),
                offset_bytes: e.offset_bytes,
                format: e.format,
                step_mode: e.step_mode,
                instance_step_rate: e.step_rate,
            })
            .collect();

        // Deterministic ordering makes generated WGSL stable (useful for caching and tests).
        attributes.sort_by_key(|a| (a.shader_location, a.pulling_slot, a.offset_bytes));

        Ok(Self {
            d3d_slot_to_pulling_slot,
            pulling_slot_to_d3d_slot,
            attributes,
        })
    }

    pub fn slot_count(&self) -> u32 {
        self.pulling_slot_to_d3d_slot.len() as u32
    }

    /// Returns the expected size in bytes of the uniform buffer for this layout.
    ///
    /// Layout:
    /// - `slots: array<IaSlot, N>` where `IaSlot` is 16 bytes in `@group var<uniform>` layout.
    /// - `draw: vec4<u32>` (16 bytes) holding draw parameters.
    pub fn uniform_size_bytes(&self) -> u64 {
        16u64 * self.slot_count() as u64 + 16u64
    }

    /// Pack slot state + draw parameters into the uniform bytes expected by the WGSL emitted by
    /// [`Self::wgsl_prelude`].
    pub fn pack_uniform_bytes(
        &self,
        slots: &[VertexPullingSlot],
        draw: VertexPullingDrawParams,
    ) -> Vec<u8> {
        assert_eq!(
            slots.len(),
            self.pulling_slot_to_d3d_slot.len(),
            "slot uniform data length must match layout slot_count"
        );

        let mut out = Vec::with_capacity(self.uniform_size_bytes() as usize);
        for s in slots {
            out.extend_from_slice(&s.base_offset_bytes.to_le_bytes());
            out.extend_from_slice(&s.stride_bytes.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        out.extend_from_slice(&draw.first_vertex.to_le_bytes());
        out.extend_from_slice(&draw.first_instance.to_le_bytes());
        out.extend_from_slice(&draw.base_vertex.to_le_bytes());
        out.extend_from_slice(&draw.first_index.to_le_bytes());
        out
    }

    /// Emit WGSL declarations + helper functions for vertex pulling.
    ///
    /// This does not include any stage entry points.
    pub fn wgsl_prelude(&self) -> String {
        let slot_count = self.slot_count();
        let mut s = String::new();

        s.push_str("// ---- Aero vertex pulling (generated) ----\n");
        s.push_str(
            "struct AeroVpIaSlot {\n  base_offset_bytes: u32,\n  stride_bytes: u32,\n  _pad0: u32,\n  _pad1: u32,\n};\n\n",
        );
        s.push_str(&format!(
            "struct AeroVpIaUniform {{\n  slots: array<AeroVpIaSlot, {slot_count}>,\n  first_vertex: u32,\n  first_instance: u32,\n  base_vertex: i32,\n  first_index: u32,\n}};\n\n"
        ));

        // Vertex buffers.
        for slot in 0..slot_count {
            let binding = VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + slot;
            s.push_str(&format!(
                "@group({}) @binding({}) var<storage, read> aero_vp_vb{}: array<u32>;\n",
                VERTEX_PULLING_GROUP, binding, slot
            ));
        }
        s.push_str(&format!(
            "@group({}) @binding({}) var<uniform> aero_vp_ia: AeroVpIaUniform;\n\n",
            VERTEX_PULLING_GROUP, VERTEX_PULLING_UNIFORM_BINDING
        ));

        // Raw u32 loads.
        s.push_str("fn aero_vp_load_u32(slot: u32, addr_bytes: u32) -> u32 {\n");
        s.push_str("  let word_index: u32 = addr_bytes >> 2u;\n");
        s.push_str("  let shift: u32 = (addr_bytes & 3u) * 8u;\n");
        s.push_str("  switch slot {\n");
        for slot in 0..slot_count {
            s.push_str(&format!("    case {slot}u: {{\n"));
            s.push_str(&format!(
                "      let word_count: u32 = arrayLength(&aero_vp_vb{slot});\n"
            ));
            s.push_str("      if (word_index >= word_count) { return 0u; }\n");
            s.push_str(&format!(
                "      let lo: u32 = aero_vp_vb{slot}[word_index];\n"
            ));
            s.push_str("      if (shift == 0u) { return lo; }\n");
            s.push_str(&format!(
                "      let hi: u32 = select(0u, aero_vp_vb{slot}[word_index + 1u], (word_index + 1u) < word_count);\n"
            ));
            s.push_str("      return (lo >> shift) | (hi << (32u - shift));\n");
            s.push_str("    }\n");
        }
        s.push_str("    default: { return 0u; }\n");
        s.push_str("  }\n");
        s.push_str("}\n\n");

        // Typed loads.
        s.push_str(
            "fn load_attr_f32(slot: u32, addr_bytes: u32) -> f32 {\n  return bitcast<f32>(aero_vp_load_u32(slot, addr_bytes));\n}\n\n",
        );
        s.push_str(
            "fn load_attr_f32x2(slot: u32, addr_bytes: u32) -> vec2<f32> {\n  let w0 = aero_vp_load_u32(slot, addr_bytes);\n  let w1 = aero_vp_load_u32(slot, addr_bytes + 4u);\n  return vec2<f32>(bitcast<f32>(w0), bitcast<f32>(w1));\n}\n\n",
        );
        s.push_str(
            "fn load_attr_f32x3(slot: u32, addr_bytes: u32) -> vec3<f32> {\n  let w0 = aero_vp_load_u32(slot, addr_bytes);\n  let w1 = aero_vp_load_u32(slot, addr_bytes + 4u);\n  let w2 = aero_vp_load_u32(slot, addr_bytes + 8u);\n  return vec3<f32>(bitcast<f32>(w0), bitcast<f32>(w1), bitcast<f32>(w2));\n}\n\n",
        );
        s.push_str(
            "fn load_attr_f32x4(slot: u32, addr_bytes: u32) -> vec4<f32> {\n  let w0 = aero_vp_load_u32(slot, addr_bytes);\n  let w1 = aero_vp_load_u32(slot, addr_bytes + 4u);\n  let w2 = aero_vp_load_u32(slot, addr_bytes + 8u);\n  let w3 = aero_vp_load_u32(slot, addr_bytes + 12u);\n  return vec4<f32>(bitcast<f32>(w0), bitcast<f32>(w1), bitcast<f32>(w2), bitcast<f32>(w3));\n}\n\n",
        );
        s.push_str(
            "fn load_attr_unorm8x4(slot: u32, addr_bytes: u32) -> vec4<f32> {\n  let w = aero_vp_load_u32(slot, addr_bytes);\n  let r = f32(w & 0xFFu) / 255.0;\n  let g = f32((w >> 8u) & 0xFFu) / 255.0;\n  let b = f32((w >> 16u) & 0xFFu) / 255.0;\n  let a = f32((w >> 24u) & 0xFFu) / 255.0;\n  return vec4<f32>(r, g, b, a);\n}\n\n",
        );
        s.push_str(
            "fn load_attr_unorm10_10_10_2(slot: u32, addr_bytes: u32) -> vec4<f32> {\n  let w = aero_vp_load_u32(slot, addr_bytes);\n  let r = f32(w & 0x3FFu) / 1023.0;\n  let g = f32((w >> 10u) & 0x3FFu) / 1023.0;\n  let b = f32((w >> 20u) & 0x3FFu) / 1023.0;\n  let a = f32((w >> 30u) & 0x3u) / 3.0;\n  return vec4<f32>(r, g, b, a);\n}\n\n",
        );

        s
    }

    /// Create a bind-group layout matching the resources declared by [`Self::wgsl_prelude`].
    pub fn create_bind_group_layout(&self, device: &wgpu::Device) -> wgpu::BindGroupLayout {
        let entries = self.bind_group_layout_entries();
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero vertex pulling bind group layout"),
            entries: &entries,
        })
    }

    /// Bind group layout entries matching [`Self::wgsl_prelude`].
    ///
    /// This is useful when callers want to use an external bind-group-layout cache.
    pub fn bind_group_layout_entries(&self) -> Vec<wgpu::BindGroupLayoutEntry> {
        let slot_count = self.slot_count();
        let mut entries: Vec<wgpu::BindGroupLayoutEntry> =
            Vec::with_capacity(slot_count as usize + 1);
        for slot in 0..slot_count {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + slot,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: VERTEX_PULLING_UNIFORM_BINDING,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        entries
    }

    /// Create a bind group for IA vertex pulling.
    ///
    /// `vertex_buffers` are provided in pulling-slot order (0..N), i.e. using the compaction order
    /// of [`Self::pulling_slot_to_d3d_slot`].
    pub fn create_bind_group(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        vertex_buffers: &[&wgpu::Buffer],
        uniform_buffer: wgpu::BufferBinding<'_>,
    ) -> wgpu::BindGroup {
        assert_eq!(
            vertex_buffers.len(),
            self.pulling_slot_to_d3d_slot.len(),
            "vertex_buffers length must match slot_count"
        );

        let mut entries: Vec<wgpu::BindGroupEntry<'_>> =
            Vec::with_capacity(vertex_buffers.len() + 1);
        for (slot, buf) in vertex_buffers.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + slot as u32,
                resource: buf.as_entire_binding(),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: VERTEX_PULLING_UNIFORM_BINDING,
            resource: wgpu::BindingResource::Buffer(uniform_buffer),
        });

        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero vertex pulling bind group"),
            layout,
            entries: &entries,
        })
    }
}
