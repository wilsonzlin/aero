use std::collections::BTreeMap;
use std::sync::Arc;

use aero_gpu::bindings::bind_group_cache::{
    BindGroupCache, BindGroupCacheEntry, BindGroupCacheResource, BufferId, TextureViewId,
};
use aero_gpu::bindings::layout_cache::{BindGroupLayoutCache, CachedBindGroupLayout};
use aero_gpu::bindings::samplers::CachedSampler;
use aero_gpu::pipeline_key::PipelineLayoutKey;
use anyhow::{bail, Result};

use crate::binding_model::{
    BINDING_BASE_CBUFFER, BINDING_BASE_INTERNAL, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE,
    BINDING_BASE_UAV, BIND_GROUP_INTERNAL_EMULATION, MAX_CBUFFER_SLOTS, MAX_SAMPLER_SLOTS,
    MAX_TEXTURE_SLOTS, MAX_UAV_SLOTS,
};

/// The AeroGPU D3D11 binding model uses stage-scoped bind groups:
/// - `@group(0)` = vertex shader resources
/// - `@group(1)` = pixel/fragment shader resources
/// - `@group(2)` = compute shader resources
/// - `@group(3)` = D3D11 extended stage resources (GS/HS/DS; executed via compute emulation)
///
/// Note: `@group(3)` is shared by multiple emulated stages. When running an emulation compute pass,
/// the executor decides which D3D11 stage bucket (`GS`/`HS`/`DS`) `@group(3)` should source bindings
/// from for that pass.
///
/// These groups are reserved for guest-translated shaders. Internal/emulation
/// pipelines may opt into additional bind groups beyond this range, but guest
/// shaders must remain constrained to `0..=MAX_GUEST_BIND_GROUP_INDEX`.
const MAX_GUEST_BIND_GROUP_INDEX: u32 = BIND_GROUP_INTERNAL_EMULATION;
pub(super) const UNIFORM_BINDING_SIZE_ALIGN: u64 = 16;
const STORAGE_BINDING_SIZE_ALIGN: u64 = 4;

fn clamp_storage_buffer_slice(
    offset: u64,
    size: Option<u64>,
    total_size: u64,
    min_offset_alignment: u64,
    max_binding_size: u64,
) -> Option<(u64, u64)> {
    if offset >= total_size {
        return None;
    }
    if offset != 0 && min_offset_alignment != 0 && !offset.is_multiple_of(min_offset_alignment) {
        return None;
    }

    let remaining = total_size - offset;
    let mut bind_size = size.unwrap_or(remaining).min(remaining);
    bind_size = bind_size.min(max_binding_size);
    bind_size -= bind_size % STORAGE_BINDING_SIZE_ALIGN;
    if bind_size == 0 {
        return None;
    }

    Some((offset, bind_size))
}

fn dummy_storage_buffer_slice_size(max_binding_size: u64) -> Option<u64> {
    let mut size = STORAGE_BINDING_SIZE_ALIGN.min(max_binding_size);
    size -= size % STORAGE_BINDING_SIZE_ALIGN;
    (size != 0).then_some(size)
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ShaderBindingSet<'a> {
    /// Bindings derived from guest-translated shaders (DXBC → WGSL).
    ///
    /// These are validated strictly and may only use `@group(0..=MAX_GUEST_BIND_GROUP_INDEX)`.
    Guest(&'a [crate::Binding]),
    /// Bindings defined by internal/emulation WGSL shaders.
    ///
    /// These are only accepted when the caller opts into allowing internal bind
    /// groups via [`BindGroupIndexValidation`].
    #[allow(dead_code)]
    Internal(&'a [crate::Binding]),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum BindGroupIndexValidation {
    /// Allow only the stage-scoped guest bind groups `@group(0..=MAX_GUEST_BIND_GROUP_INDEX)`.
    GuestShaders,
    /// Allow internal/emulation bindings to use bind groups up to (and including)
    /// `max_internal_bind_group_index`.
    ///
    /// Guest shader bindings are still limited to `@group(0..=MAX_GUEST_BIND_GROUP_INDEX)`.
    #[allow(dead_code)]
    GuestAndInternal { max_internal_bind_group_index: u32 },
}

#[derive(Debug, Clone)]
pub(super) struct PipelineBindingsInfo {
    pub layout_key: PipelineLayoutKey,
    pub group_layouts: Vec<CachedBindGroupLayout>,
    pub group_bindings: Vec<Vec<crate::Binding>>,
}

pub(super) fn build_pipeline_bindings_info<'a, I>(
    device: &wgpu::Device,
    bind_group_layout_cache: &mut BindGroupLayoutCache,
    shader_bindings: I,
    bind_group_validation: BindGroupIndexValidation,
) -> Result<PipelineBindingsInfo>
where
    I: IntoIterator<Item = ShaderBindingSet<'a>>,
{
    let features = device.features();
    let max_uniform_binding_size = device.limits().max_uniform_buffer_binding_size as u64;
    let max_storage_buffers_per_shader_stage = device.limits().max_storage_buffers_per_shader_stage;
    let max_uniform_buffers_per_shader_stage = device.limits().max_uniform_buffers_per_shader_stage;
    let max_sampled_textures_per_shader_stage =
        device.limits().max_sampled_textures_per_shader_stage;
    let max_samplers_per_shader_stage = device.limits().max_samplers_per_shader_stage;
    let max_storage_textures_per_shader_stage =
        device.limits().max_storage_textures_per_shader_stage;

    let max_internal_bind_group_index = match bind_group_validation {
        BindGroupIndexValidation::GuestShaders => None,
        BindGroupIndexValidation::GuestAndInternal {
            max_internal_bind_group_index,
        } => {
            if max_internal_bind_group_index < MAX_GUEST_BIND_GROUP_INDEX {
                bail!(
                        "BindGroupIndexValidation::GuestAndInternal max_internal_bind_group_index must be >= {MAX_GUEST_BIND_GROUP_INDEX} (got {max_internal_bind_group_index})"
                    );
            }
            Some(max_internal_bind_group_index)
        }
    };

    let mut groups: BTreeMap<u32, BTreeMap<u32, crate::Binding>> = BTreeMap::new();
    for shader in shader_bindings {
        let (shader_kind, max_group, bindings) = match shader {
            ShaderBindingSet::Guest(bindings) => ("guest", MAX_GUEST_BIND_GROUP_INDEX, bindings),
            ShaderBindingSet::Internal(bindings) => {
                let Some(max_internal) = max_internal_bind_group_index else {
                    bail!("internal bind groups are disabled; caller must opt into BindGroupIndexValidation::GuestAndInternal");
                };
                ("internal", max_internal, bindings)
            }
        };

        for binding in bindings {
            if binding.group > max_group {
                bail!(
                    "{shader_kind} binding @group({}) is out of range for AeroGPU D3D11 binding model (max {max_group})",
                    binding.group,
                );
            }

            if max_storage_buffers_per_shader_stage == 0
                && matches!(
                    binding.kind,
                    crate::BindingKind::SrvBuffer { .. }
                        | crate::BindingKind::UavBuffer { .. }
                        | crate::BindingKind::ExpansionStorageBuffer { .. }
                )
            {
                bail!(
                    "{shader_kind} binding @group({}) @binding({}) requires storage buffers, but this device reports max_storage_buffers_per_shader_stage=0",
                    binding.group,
                    binding.binding,
                );
            }

            if max_storage_textures_per_shader_stage == 0
                && matches!(
                    binding.kind,
                    crate::BindingKind::UavTexture2DWriteOnly { .. }
                )
            {
                bail!(
                    "{shader_kind} binding @group({}) @binding({}) requires storage textures, but this device reports max_storage_textures_per_shader_stage=0",
                    binding.group,
                    binding.binding,
                );
            }

            // WebGPU only allows writable storage buffers/textures in the compute stage. wgpu
            // exposes optional native-only features to enable writable storage in vertex/fragment
            // stages. If those features are absent, fail fast with a clear diagnostic rather than
            // triggering a wgpu validation panic during pipeline creation.
            let writable_storage = matches!(
                binding.kind,
                crate::BindingKind::UavBuffer { .. }
                    | crate::BindingKind::UavTexture2DWriteOnly { .. }
                    | crate::BindingKind::ExpansionStorageBuffer { read_only: false }
            );
            if writable_storage {
                if binding.visibility.contains(wgpu::ShaderStages::VERTEX)
                    && !features.contains(wgpu::Features::VERTEX_WRITABLE_STORAGE)
                {
                    bail!(
                        "{shader_kind} binding @group({}) @binding({}) uses writable storage in vertex stage, but device does not support wgpu::Features::VERTEX_WRITABLE_STORAGE",
                        binding.group,
                        binding.binding
                    );
                }
                if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    bail!(
                        "{shader_kind} binding @group({}) @binding({}) uses writable storage in fragment stage, which is not supported by this wgpu/WebGPU build",
                        binding.group,
                        binding.binding
                    );
                }
            }

            if let crate::BindingKind::ConstantBuffer { slot, reg_count } = binding.kind {
                let required_min = (reg_count as u64)
                    .saturating_mul(UNIFORM_BINDING_SIZE_ALIGN)
                    .max(UNIFORM_BINDING_SIZE_ALIGN);
                if required_min > max_uniform_binding_size {
                    bail!(
                        "cbuffer slot {slot} requires {required_min} bytes, which exceeds device limit max_uniform_buffer_binding_size={max_uniform_binding_size}"
                    );
                }
            }
            let group_map = groups.entry(binding.group).or_default();
            if let Some(existing) = group_map.get_mut(&binding.binding) {
                if existing.kind != binding.kind {
                    bail!(
                        "binding @group({}) @binding({}) kind mismatch across shaders ({:?} vs {:?})",
                        binding.group,
                        binding.binding,
                        existing.kind,
                        binding.kind,
                    );
                }
                existing.visibility |= binding.visibility;
            } else {
                group_map.insert(binding.binding, *binding);
            }
        }
    }

    if groups.is_empty() {
        return Ok(PipelineBindingsInfo {
            layout_key: PipelineLayoutKey::empty(),
            group_layouts: Vec::new(),
            group_bindings: Vec::new(),
        });
    }

    let max_bindings_per_bind_group = device.limits().max_bindings_per_bind_group as usize;
    for (group_index, bindings) in &groups {
        let count = bindings.len();
        if count > max_bindings_per_bind_group {
            bail!(
                "pipeline requires {count} bindings in @group({group_index}), but device limit max_bindings_per_bind_group={max_bindings_per_bind_group}"
            );
        }
    }

    // wgpu enforces per-stage resource limits. Validate early so callers get a clear error rather
    // than a backend validation panic.
    let mut uniform_buffers_vertex = 0u32;
    let mut uniform_buffers_fragment = 0u32;
    let mut uniform_buffers_compute = 0u32;
    let mut sampled_textures_vertex = 0u32;
    let mut sampled_textures_fragment = 0u32;
    let mut sampled_textures_compute = 0u32;
    let mut samplers_vertex = 0u32;
    let mut samplers_fragment = 0u32;
    let mut samplers_compute = 0u32;
    let mut storage_buffers_vertex = 0u32;
    let mut storage_buffers_fragment = 0u32;
    let mut storage_buffers_compute = 0u32;
    let mut storage_textures_vertex = 0u32;
    let mut storage_textures_fragment = 0u32;
    let mut storage_textures_compute = 0u32;
    for group in groups.values() {
        for binding in group.values() {
            let is_uniform_buffer =
                matches!(binding.kind, crate::BindingKind::ConstantBuffer { .. });
            let is_sampled_texture = matches!(
                binding.kind,
                crate::BindingKind::Texture2D { .. } | crate::BindingKind::Texture2DArray { .. }
            );
            let is_sampler = matches!(binding.kind, crate::BindingKind::Sampler { .. });
            let is_storage_buffer = matches!(
                binding.kind,
                crate::BindingKind::SrvBuffer { .. }
                    | crate::BindingKind::UavBuffer { .. }
                    | crate::BindingKind::ExpansionStorageBuffer { .. }
            );
            let is_storage_texture = matches!(
                binding.kind,
                crate::BindingKind::UavTexture2DWriteOnly { .. }
            );
            if is_uniform_buffer {
                if binding.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    uniform_buffers_vertex = uniform_buffers_vertex.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    uniform_buffers_fragment = uniform_buffers_fragment.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    uniform_buffers_compute = uniform_buffers_compute.saturating_add(1);
                }
            }
            if is_sampled_texture {
                if binding.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    sampled_textures_vertex = sampled_textures_vertex.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    sampled_textures_fragment = sampled_textures_fragment.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    sampled_textures_compute = sampled_textures_compute.saturating_add(1);
                }
            }
            if is_sampler {
                if binding.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    samplers_vertex = samplers_vertex.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    samplers_fragment = samplers_fragment.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    samplers_compute = samplers_compute.saturating_add(1);
                }
            }
            if is_storage_buffer {
                if binding.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    storage_buffers_vertex = storage_buffers_vertex.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    storage_buffers_fragment = storage_buffers_fragment.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    storage_buffers_compute = storage_buffers_compute.saturating_add(1);
                }
            }
            if is_storage_texture {
                if binding.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    storage_textures_vertex = storage_textures_vertex.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    storage_textures_fragment = storage_textures_fragment.saturating_add(1);
                }
                if binding.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    storage_textures_compute = storage_textures_compute.saturating_add(1);
                }
            }
        }
    }
    if uniform_buffers_vertex > max_uniform_buffers_per_shader_stage {
        bail!(
            "pipeline uses {uniform_buffers_vertex} uniform buffers in vertex stage, but device limit max_uniform_buffers_per_shader_stage={max_uniform_buffers_per_shader_stage}"
        );
    }
    if uniform_buffers_fragment > max_uniform_buffers_per_shader_stage {
        bail!(
            "pipeline uses {uniform_buffers_fragment} uniform buffers in fragment stage, but device limit max_uniform_buffers_per_shader_stage={max_uniform_buffers_per_shader_stage}"
        );
    }
    if uniform_buffers_compute > max_uniform_buffers_per_shader_stage {
        bail!(
            "pipeline uses {uniform_buffers_compute} uniform buffers in compute stage, but device limit max_uniform_buffers_per_shader_stage={max_uniform_buffers_per_shader_stage}"
        );
    }
    if sampled_textures_vertex > max_sampled_textures_per_shader_stage {
        bail!(
            "pipeline uses {sampled_textures_vertex} sampled textures in vertex stage, but device limit max_sampled_textures_per_shader_stage={max_sampled_textures_per_shader_stage}"
        );
    }
    if sampled_textures_fragment > max_sampled_textures_per_shader_stage {
        bail!(
            "pipeline uses {sampled_textures_fragment} sampled textures in fragment stage, but device limit max_sampled_textures_per_shader_stage={max_sampled_textures_per_shader_stage}"
        );
    }
    if sampled_textures_compute > max_sampled_textures_per_shader_stage {
        bail!(
            "pipeline uses {sampled_textures_compute} sampled textures in compute stage, but device limit max_sampled_textures_per_shader_stage={max_sampled_textures_per_shader_stage}"
        );
    }
    if samplers_vertex > max_samplers_per_shader_stage {
        bail!(
            "pipeline uses {samplers_vertex} samplers in vertex stage, but device limit max_samplers_per_shader_stage={max_samplers_per_shader_stage}"
        );
    }
    if samplers_fragment > max_samplers_per_shader_stage {
        bail!(
            "pipeline uses {samplers_fragment} samplers in fragment stage, but device limit max_samplers_per_shader_stage={max_samplers_per_shader_stage}"
        );
    }
    if samplers_compute > max_samplers_per_shader_stage {
        bail!(
            "pipeline uses {samplers_compute} samplers in compute stage, but device limit max_samplers_per_shader_stage={max_samplers_per_shader_stage}"
        );
    }
    if storage_buffers_vertex > max_storage_buffers_per_shader_stage {
        bail!(
            "pipeline uses {storage_buffers_vertex} storage buffers in vertex stage, but device limit max_storage_buffers_per_shader_stage={max_storage_buffers_per_shader_stage}"
        );
    }
    if storage_buffers_fragment > max_storage_buffers_per_shader_stage {
        bail!(
            "pipeline uses {storage_buffers_fragment} storage buffers in fragment stage, but device limit max_storage_buffers_per_shader_stage={max_storage_buffers_per_shader_stage}"
        );
    }
    if storage_buffers_compute > max_storage_buffers_per_shader_stage {
        bail!(
            "pipeline uses {storage_buffers_compute} storage buffers in compute stage, but device limit max_storage_buffers_per_shader_stage={max_storage_buffers_per_shader_stage}"
        );
    }
    if storage_textures_vertex > max_storage_textures_per_shader_stage {
        bail!(
            "pipeline uses {storage_textures_vertex} storage textures in vertex stage, but device limit max_storage_textures_per_shader_stage={max_storage_textures_per_shader_stage}"
        );
    }
    if storage_textures_fragment > max_storage_textures_per_shader_stage {
        bail!(
            "pipeline uses {storage_textures_fragment} storage textures in fragment stage, but device limit max_storage_textures_per_shader_stage={max_storage_textures_per_shader_stage}"
        );
    }
    if storage_textures_compute > max_storage_textures_per_shader_stage {
        bail!(
            "pipeline uses {storage_textures_compute} storage textures in compute stage, but device limit max_storage_textures_per_shader_stage={max_storage_textures_per_shader_stage}"
        );
    }

    let max_group = groups
        .keys()
        .copied()
        .max()
        .expect("groups.is_empty handled above");

    let max_bind_groups = device.limits().max_bind_groups;
    if max_group >= max_bind_groups {
        bail!(
            "pipeline requires bind group indices 0..={max_group} ({} total), but device supports max_bind_groups={max_bind_groups}",
            max_group.saturating_add(1)
        );
    }
    let mut group_layouts = Vec::with_capacity(max_group as usize + 1);
    let mut group_bindings = Vec::with_capacity(max_group as usize + 1);

    for group_index in 0..=max_group {
        let bindings: Vec<crate::Binding> = groups
            .get(&group_index)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();

        let mut entries = Vec::with_capacity(bindings.len());
        for binding in &bindings {
            entries.push(binding_to_layout_entry(binding)?);
        }

        let layout = bind_group_layout_cache.get_or_create(device, &entries);
        group_layouts.push(layout);
        group_bindings.push(bindings);
    }

    let layout_key = PipelineLayoutKey {
        bind_group_layout_hashes: group_layouts.iter().map(|l| l.hash).collect(),
    };

    Ok(PipelineBindingsInfo {
        layout_key,
        group_layouts,
        group_bindings,
    })
}

pub(super) fn binding_to_layout_entry(
    binding: &crate::Binding,
) -> Result<wgpu::BindGroupLayoutEntry> {
    let ty = match &binding.kind {
        crate::BindingKind::ConstantBuffer { slot, reg_count } => {
            if *slot >= MAX_CBUFFER_SLOTS {
                bail!(
                    "cbuffer slot {slot} is out of range for binding model (max {})",
                    MAX_CBUFFER_SLOTS - 1
                );
            }
            let expected = BINDING_BASE_CBUFFER + slot;
            if binding.binding != expected {
                bail!(
                    "cbuffer slot {slot} expected @binding({expected}), got {}",
                    binding.binding
                );
            }
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new((*reg_count as u64) * 16),
            }
        }
        crate::BindingKind::Texture2D { slot } => {
            if *slot >= MAX_TEXTURE_SLOTS {
                bail!(
                    "texture slot {slot} is out of range for binding model (max {})",
                    MAX_TEXTURE_SLOTS - 1
                );
            }
            let expected = BINDING_BASE_TEXTURE + slot;
            if binding.binding != expected {
                bail!(
                    "texture slot {slot} expected @binding({expected}), got {}",
                    binding.binding
                );
            }
            wgpu::BindingType::Texture {
                multisampled: false,
                view_dimension: wgpu::TextureViewDimension::D2,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
            }
        }
        crate::BindingKind::Texture2DArray { slot } => {
            if *slot >= MAX_TEXTURE_SLOTS {
                bail!(
                    "texture slot {slot} is out of range for binding model (max {})",
                    MAX_TEXTURE_SLOTS - 1
                );
            }
            let expected = BINDING_BASE_TEXTURE + slot;
            if binding.binding != expected {
                bail!(
                    "texture slot {slot} expected @binding({expected}), got {}",
                    binding.binding
                );
            }
            wgpu::BindingType::Texture {
                multisampled: false,
                view_dimension: wgpu::TextureViewDimension::D2Array,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
            }
        }
        crate::BindingKind::SrvBuffer { slot } => {
            if *slot >= MAX_TEXTURE_SLOTS {
                bail!(
                    "srv buffer slot {slot} is out of range for binding model (max {})",
                    MAX_TEXTURE_SLOTS - 1
                );
            }
            let expected = BINDING_BASE_TEXTURE + slot;
            if binding.binding != expected {
                bail!(
                    "srv buffer slot {slot} expected @binding({expected}), got {}",
                    binding.binding
                );
            }
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            }
        }
        crate::BindingKind::Sampler { slot } => {
            if *slot >= MAX_SAMPLER_SLOTS {
                bail!(
                    "sampler slot {slot} is out of range for binding model (max {})",
                    MAX_SAMPLER_SLOTS - 1
                );
            }
            let expected = BINDING_BASE_SAMPLER + slot;
            if binding.binding != expected {
                bail!(
                    "sampler slot {slot} expected @binding({expected}), got {}",
                    binding.binding
                );
            }
            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
        }
        crate::BindingKind::UavBuffer { slot } => {
            if *slot >= MAX_UAV_SLOTS {
                bail!(
                    "uav buffer slot {slot} is out of range for binding model (max {})",
                    MAX_UAV_SLOTS - 1
                );
            }
            let expected = BINDING_BASE_UAV + slot;
            if binding.binding != expected {
                bail!(
                    "uav buffer slot {slot} expected @binding({expected}), got {}",
                    binding.binding
                );
            }
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            }
        }
        crate::BindingKind::UavTexture2DWriteOnly { slot, format } => {
            if *slot >= MAX_UAV_SLOTS {
                bail!(
                    "uav slot {slot} is out of range for binding model (max {})",
                    MAX_UAV_SLOTS - 1
                );
            }
            let expected = BINDING_BASE_UAV + slot;
            if binding.binding != expected {
                bail!(
                    "uav slot {slot} expected @binding({expected}), got {}",
                    binding.binding
                );
            }
            wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: format.wgpu_format(),
                view_dimension: wgpu::TextureViewDimension::D2,
            }
        }
        crate::BindingKind::ExpansionStorageBuffer { read_only } => {
            if binding.binding < BINDING_BASE_INTERNAL {
                bail!(
                    "expansion storage buffer binding @binding({}) must be >= BINDING_BASE_INTERNAL ({BINDING_BASE_INTERNAL})",
                    binding.binding
                );
            }
            if binding.group != 3 {
                bail!(
                    "expansion storage buffer bindings must live in @group(3), got @group({}) @binding({})",
                    binding.group,
                    binding.binding
                );
            }
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage {
                    read_only: *read_only,
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            }
        }
    };

    Ok(wgpu::BindGroupLayoutEntry {
        binding: binding.binding,
        visibility: binding.visibility,
        ty,
        count: None,
    })
}

pub(super) struct BufferBinding<'a> {
    pub id: BufferId,
    pub buffer: &'a wgpu::Buffer,
    pub offset: u64,
    pub size: Option<u64>,
    pub total_size: u64,
}

pub(super) trait BindGroupResourceProvider {
    fn constant_buffer(&self, slot: u32) -> Option<BufferBinding<'_>>;
    fn constant_buffer_scratch(&self, slot: u32) -> Option<(BufferId, &wgpu::Buffer)>;
    #[allow(dead_code)]
    fn storage_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
        None
    }
    fn texture2d(&self, slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)>;
    fn texture2d_array(&self, slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)>;
    fn sampler(&self, slot: u32) -> Option<&CachedSampler>;

    /// Optional `t#` SRV buffer binding.
    fn srv_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
        None
    }

    /// Optional `u#` UAV buffer binding.
    fn uav_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
        None
    }
    fn internal_buffer(&self, _binding: u32) -> Option<BufferBinding<'_>> {
        None
    }

    /// Optional `u#` UAV texture binding.
    fn uav_texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
        None
    }

    fn dummy_uniform(&self) -> &wgpu::Buffer;
    fn dummy_storage(&self) -> &wgpu::Buffer;
    fn dummy_storage_texture_view(
        &self,
        format: crate::StorageTextureFormat,
    ) -> &wgpu::TextureView;
    fn dummy_texture_view_2d(&self) -> &wgpu::TextureView;
    fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView;
    fn default_sampler(&self) -> &CachedSampler;
}

pub(super) fn build_bind_group(
    device: &wgpu::Device,
    cache: &mut BindGroupCache<Arc<wgpu::BindGroup>>,
    layout: &CachedBindGroupLayout,
    bindings: &[crate::Binding],
    provider: &impl BindGroupResourceProvider,
) -> Result<Arc<wgpu::BindGroup>> {
    let uniform_align = device.limits().min_uniform_buffer_offset_alignment as u64;
    let max_uniform_binding_size = device.limits().max_uniform_buffer_binding_size as u64;
    let storage_align = device.limits().min_storage_buffer_offset_alignment as u64;
    let max_storage_binding_size = device.limits().max_storage_buffer_binding_size as u64;

    let mut entries: Vec<BindGroupCacheEntry<'_>> = Vec::with_capacity(bindings.len());
    for binding in bindings {
        match &binding.kind {
            crate::BindingKind::ConstantBuffer { slot, reg_count } => {
                let mut id = BufferId(0);
                let mut buffer = provider.dummy_uniform();
                let mut offset = 0;
                let mut size: Option<u64> = None;
                let mut total_size = 0u64;

                if let Some(bound) = provider.constant_buffer(*slot) {
                    id = bound.id;
                    buffer = bound.buffer;
                    offset = bound.offset;
                    size = bound.size;
                    total_size = bound.total_size;
                }

                if id != BufferId(0) {
                    let required_min = (*reg_count as u64)
                        .saturating_mul(UNIFORM_BINDING_SIZE_ALIGN)
                        .max(UNIFORM_BINDING_SIZE_ALIGN);

                    if offset >= total_size {
                        id = BufferId(0);
                        buffer = provider.dummy_uniform();
                        offset = 0;
                        size = None;
                    } else {
                        let remaining = total_size - offset;
                        let mut bind_size = size.unwrap_or(remaining).min(remaining);
                        if bind_size < required_min {
                            id = BufferId(0);
                            buffer = provider.dummy_uniform();
                            offset = 0;
                            size = None;
                        } else {
                            if bind_size > max_uniform_binding_size {
                                bind_size = max_uniform_binding_size;
                            }
                            // WebGPU requires uniform buffer binding sizes to be 16-byte aligned.
                            bind_size -= bind_size % UNIFORM_BINDING_SIZE_ALIGN;
                            if bind_size < required_min {
                                id = BufferId(0);
                                buffer = provider.dummy_uniform();
                                offset = 0;
                                size = None;
                            } else if offset != 0 && !offset.is_multiple_of(uniform_align) {
                                if let Some((scratch_id, scratch_buffer)) =
                                    provider.constant_buffer_scratch(*slot)
                                {
                                    id = scratch_id;
                                    buffer = scratch_buffer;
                                    offset = 0;
                                    size = Some(bind_size);
                                } else {
                                    id = BufferId(0);
                                    buffer = provider.dummy_uniform();
                                    offset = 0;
                                    size = None;
                                }
                            } else {
                                size = Some(bind_size);
                            }
                        }
                    }
                }

                if id == BufferId(0) {
                    // Bind a slice of the dummy uniform rather than the full buffer. This keeps the
                    // binding size within WebGPU limits even if the dummy uniform is larger than
                    // `max_uniform_buffer_binding_size` on some backends.
                    let required_min = (*reg_count as u64)
                        .saturating_mul(UNIFORM_BINDING_SIZE_ALIGN)
                        .max(UNIFORM_BINDING_SIZE_ALIGN);
                    let slice_size = required_min
                        .max(UNIFORM_BINDING_SIZE_ALIGN)
                        .min(max_uniform_binding_size);
                    size = Some(slice_size);
                }

                entries.push(BindGroupCacheEntry {
                    binding: binding.binding,
                    resource: BindGroupCacheResource::Buffer {
                        id,
                        buffer,
                        offset,
                        size: size.and_then(wgpu::BufferSize::new),
                    },
                });
            }
            crate::BindingKind::SrvBuffer { .. } | crate::BindingKind::UavBuffer { .. } => {
                let mut id = BufferId(0);
                let mut buffer = provider.dummy_storage();
                let mut offset = 0;
                let mut size: Option<u64> = None;
                let mut total_size = 0u64;

                let bound = match &binding.kind {
                    crate::BindingKind::SrvBuffer { slot } => provider.srv_buffer(*slot),
                    crate::BindingKind::UavBuffer { slot } => provider.uav_buffer(*slot),
                    _ => None,
                };
                if let Some(bound) = bound {
                    id = bound.id;
                    buffer = bound.buffer;
                    offset = bound.offset;
                    size = bound.size;
                    total_size = bound.total_size;
                }

                if id != BufferId(0) {
                    if let Some((validated_offset, bind_size)) = clamp_storage_buffer_slice(
                        offset,
                        size,
                        total_size,
                        storage_align,
                        max_storage_binding_size,
                    ) {
                        offset = validated_offset;
                        size = Some(bind_size);
                    } else {
                        // When the view's offset/size violates WebGPU's storage buffer constraints
                        // (alignment, max binding size, etc.), fall back to a small dummy storage
                        // buffer slice. This avoids wgpu validation errors; the shader will observe
                        // zeroes instead of crashing the device.
                        id = BufferId(0);
                        buffer = provider.dummy_storage();
                        offset = 0;
                        size = None;
                    }
                }

                if id == BufferId(0) && size.is_none() {
                    size = dummy_storage_buffer_slice_size(max_storage_binding_size);
                }

                entries.push(BindGroupCacheEntry {
                    binding: binding.binding,
                    resource: BindGroupCacheResource::Buffer {
                        id,
                        buffer,
                        offset,
                        size: size.and_then(wgpu::BufferSize::new),
                    },
                });
            }
            crate::BindingKind::Texture2D { slot } => {
                let (id, view) = provider
                    .texture2d(*slot)
                    .unwrap_or((TextureViewId(0), provider.dummy_texture_view_2d()));

                entries.push(BindGroupCacheEntry {
                    binding: binding.binding,
                    resource: BindGroupCacheResource::TextureView { id, view },
                });
            }
            crate::BindingKind::Texture2DArray { slot } => {
                let (id, view) = provider
                    .texture2d_array(*slot)
                    .unwrap_or((TextureViewId(0), provider.dummy_texture_view_2d_array()));

                entries.push(BindGroupCacheEntry {
                    binding: binding.binding,
                    resource: BindGroupCacheResource::TextureView { id, view },
                });
            }
            crate::BindingKind::UavTexture2DWriteOnly { slot, format } => {
                // Prefer a dedicated UAV texture binding if the runtime provides one; otherwise
                // fall back to the regular `t#` texture binding as a best-effort mapping.
                //
                // Callers that need typed UAV writes should provide textures created with
                // `STORAGE_BINDING` usage and the correct view format.
                let (id, view) = provider
                    .uav_texture2d(*slot)
                    .or_else(|| provider.texture2d(*slot))
                    .unwrap_or((TextureViewId(0), provider.dummy_storage_texture_view(*format)));

                entries.push(BindGroupCacheEntry {
                    binding: binding.binding,
                    resource: BindGroupCacheResource::TextureView { id, view },
                });
            }
            crate::BindingKind::Sampler { slot } => {
                let sampler = provider
                    .sampler(*slot)
                    .unwrap_or_else(|| provider.default_sampler());

                entries.push(BindGroupCacheEntry {
                    binding: binding.binding,
                    resource: BindGroupCacheResource::Sampler {
                        id: sampler.id,
                        sampler: sampler.sampler.as_ref(),
                    },
                });
            }
            crate::BindingKind::ExpansionStorageBuffer { .. } => {
                let bound = provider.internal_buffer(binding.binding).ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing expansion-internal buffer for @group({}) @binding({})",
                        binding.group,
                        binding.binding
                    )
                })?;

                let id = bound.id;
                let buffer = bound.buffer;
                let offset = bound.offset;
                let mut size: Option<u64> = bound.size;
                let total_size = bound.total_size;

                if offset >= total_size || (offset != 0 && !offset.is_multiple_of(storage_align)) {
                    bail!(
                        "invalid expansion-internal buffer binding @group({}) @binding({}): offset={} total_size={} storage_align={}",
                        binding.group,
                        binding.binding,
                        offset,
                        total_size,
                        storage_align
                    );
                }

                let remaining = total_size - offset;
                let mut bind_size = size.unwrap_or(remaining).min(remaining);
                if bind_size > max_storage_binding_size {
                    bind_size = max_storage_binding_size;
                }
                // WebGPU requires storage buffer binding sizes to be 4-byte aligned.
                bind_size -= bind_size % 4;
                if bind_size == 0 {
                    bail!(
                        "invalid expansion-internal buffer binding @group({}) @binding({}): computed bind_size=0 (offset={} total_size={})",
                        binding.group,
                        binding.binding,
                        offset,
                        total_size
                    );
                }
                size = Some(bind_size);

                // No dummy fallback: expansion-internal bindings are required for the emulation
                // pipeline to function, so treat missing/invalid bindings as a hard error.
                if id == BufferId(0) {
                    bail!(
                        "invalid expansion-internal buffer binding @group({}) @binding({}): BufferId must be non-zero",
                        binding.group,
                        binding.binding
                    );
                }

                entries.push(BindGroupCacheEntry {
                    binding: binding.binding,
                    resource: BindGroupCacheResource::Buffer {
                        id,
                        buffer,
                        offset,
                        size: size.and_then(wgpu::BufferSize::new),
                    },
                });
            }
        }
    }

    Ok(cache.get_or_create(device, layout, &entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_dxbc::test_utils as dxbc_test_utils;
    use aero_gpu::bindings::samplers::SamplerCache;
    use anyhow::{anyhow, Context};
    use std::sync::Arc;

    fn require_webgpu() -> bool {
        let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
            return false;
        };
        let v = raw.trim();
        v == "1"
            || v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes")
            || v.eq_ignore_ascii_case("on")
    }

    #[test]
    fn guest_bind_group_range_includes_group3() {
        assert_eq!(
            MAX_GUEST_BIND_GROUP_INDEX, BIND_GROUP_INTERNAL_EMULATION,
            "guest binding range must include @group(3) for D3D11 extended stages (GS/HS/DS)"
        );
    }

    fn skip_or_panic(test_name: &str, reason: &str) {
        if require_webgpu() {
            panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
        }
        eprintln!("skipping {test_name}: {reason}");
    }

    async fn new_device_queue_for_tests(
    ) -> anyhow::Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);

            if needs_runtime_dir {
                let dir = std::env::temp_dir()
                    .join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::GL
            } else {
                // Prefer "native" backends; this avoids noisy platform warnings from
                // initializing GL/WAYLAND stacks in headless CI environments.
                wgpu::Backends::PRIMARY
            },
            ..Default::default()
        });

        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
        {
            Some(adapter) => Some(adapter),
            None => {
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        }
        .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

        let requested_features = super::super::negotiated_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 reflection_bindings test device"),
                    required_features: requested_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

        Ok((adapter, device, queue))
    }

    async fn read_buffer(device: &wgpu::Device, buffer: &wgpu::Buffer) -> anyhow::Result<Vec<u8>> {
        let slice = buffer.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).ok();
        });

        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);
        #[cfg(target_arch = "wasm32")]
        device.poll(wgpu::Maintain::Poll);

        receiver
            .receive()
            .await
            .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
            .context("wgpu: map_async failed")?;
        let data = slice.get_mapped_range().to_vec();
        buffer.unmap();
        Ok(data)
    }

    #[test]
    fn binding_to_layout_entry_rejects_cbuffer_slot_out_of_range() {
        let slot = MAX_CBUFFER_SLOTS;
        let binding = crate::Binding {
            group: 0,
            binding: BINDING_BASE_CBUFFER + slot,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::ConstantBuffer { slot, reg_count: 1 },
        };

        let err = binding_to_layout_entry(&binding).expect_err("slot 32 must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("cbuffer") && msg.contains("out of range") && msg.contains("max 31"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn pipeline_bindings_info_allows_geometry_group_3() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            let gs = vec![crate::Binding {
                group: 3,
                binding: BINDING_BASE_TEXTURE,
                // Geometry shaders are emulated through compute passes; the binding model marks
                // their resources as compute-visible.
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let info = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [ShaderBindingSet::Internal(gs.as_slice())],
                BindGroupIndexValidation::GuestAndInternal {
                    max_internal_bind_group_index: BIND_GROUP_INTERNAL_EMULATION,
                },
            )
            .unwrap();

            assert_eq!(info.group_layouts.len(), 4);
            assert_eq!(info.group_bindings.len(), 4);
            assert!(info.group_bindings[0].is_empty());
            assert!(info.group_bindings[1].is_empty());
            assert!(info.group_bindings[2].is_empty());
            assert_eq!(info.group_bindings[3], gs);
        });
    }

    #[test]
    fn pipeline_bindings_info_deduplicates_and_unions_visibility() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            let mut layout_cache = BindGroupLayoutCache::new();
            let device = rt.device();

            let vs = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];
            let ps = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::FRAGMENT,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let info_a = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(vs.as_slice()),
                    ShaderBindingSet::Guest(ps.as_slice()),
                ],
                BindGroupIndexValidation::GuestShaders,
            )
            .unwrap();
            let info_b = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(ps.as_slice()),
                    ShaderBindingSet::Guest(vs.as_slice()),
                ],
                BindGroupIndexValidation::GuestShaders,
            )
            .unwrap();

            assert_eq!(
                info_a.layout_key, info_b.layout_key,
                "PipelineLayoutKey should be stable regardless of shader iteration order"
            );

            assert_eq!(info_a.group_bindings.len(), 1);
            assert_eq!(info_a.group_bindings[0].len(), 1);
            assert_eq!(
                info_a.group_bindings[0][0].visibility,
                wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT
            );
        });
    }

    #[test]
    fn pipeline_bindings_info_includes_empty_groups_before_max_group() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            // Vertex shader uses no resources, but pixel shader uses group 1. WebGPU requires that
            // the pipeline layout includes all bind groups up to the maximum group index, so group
            // 0 must exist as an empty layout.
            let vs: Vec<crate::Binding> = Vec::new();
            let ps = vec![crate::Binding {
                group: 1,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::FRAGMENT,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let info = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(vs.as_slice()),
                    ShaderBindingSet::Guest(ps.as_slice()),
                ],
                BindGroupIndexValidation::GuestShaders,
            )
            .unwrap();

            assert_eq!(info.group_bindings.len(), 2);
            assert!(info.group_bindings[0].is_empty());
            assert_eq!(info.group_bindings[1].len(), 1);
            assert_eq!(info.group_bindings[1][0].group, 1);
        });
    }

    #[test]
    fn pipeline_bindings_info_includes_empty_groups_for_group2() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            let empty: Vec<crate::Binding> = Vec::new();
            let cs = vec![crate::Binding {
                group: 2,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let info = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(empty.as_slice()),
                    ShaderBindingSet::Guest(cs.as_slice()),
                ],
                BindGroupIndexValidation::GuestShaders,
            )
            .unwrap();

            assert_eq!(info.group_bindings.len(), 3);
            assert!(info.group_bindings[0].is_empty());
            assert!(info.group_bindings[1].is_empty());
            assert_eq!(info.group_bindings[2].len(), 1);
            assert_eq!(info.group_bindings[2][0].group, 2);

            assert_eq!(info.group_layouts.len(), 3);
            assert!(
                Arc::ptr_eq(&info.group_layouts[0].layout, &info.group_layouts[1].layout),
                "expected group0/group1 empty layouts to be reused"
            );
            assert_eq!(info.group_layouts[0].hash, info.group_layouts[1].hash);

            assert_eq!(info.layout_key.bind_group_layout_hashes.len(), 3);
            assert_eq!(
                info.layout_key.bind_group_layout_hashes[0],
                info.group_layouts[0].hash
            );
            assert_eq!(
                info.layout_key.bind_group_layout_hashes[1],
                info.group_layouts[1].hash
            );
            assert_eq!(
                info.layout_key.bind_group_layout_hashes[2],
                info.group_layouts[2].hash
            );
        });
    }

    #[test]
    fn pipeline_bindings_info_includes_empty_groups_for_group3() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            let empty: Vec<crate::Binding> = Vec::new();
            let gs = vec![crate::Binding {
                group: 3,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let info = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(empty.as_slice()),
                    ShaderBindingSet::Internal(gs.as_slice()),
                ],
                BindGroupIndexValidation::GuestAndInternal {
                    max_internal_bind_group_index: BIND_GROUP_INTERNAL_EMULATION,
                },
            )
            .unwrap();

            assert_eq!(info.group_bindings.len(), 4);
            assert!(info.group_bindings[0].is_empty());
            assert!(info.group_bindings[1].is_empty());
            assert!(info.group_bindings[2].is_empty());
            assert_eq!(info.group_bindings[3].len(), 1);
            assert_eq!(info.group_bindings[3][0].group, 3);

            assert_eq!(info.group_layouts.len(), 4);
            assert!(
                Arc::ptr_eq(&info.group_layouts[0].layout, &info.group_layouts[1].layout),
                "expected group0/group1 empty layouts to be reused"
            );
            assert!(
                Arc::ptr_eq(&info.group_layouts[0].layout, &info.group_layouts[2].layout),
                "expected group0/group2 empty layouts to be reused"
            );
            assert!(
                Arc::ptr_eq(&info.group_layouts[1].layout, &info.group_layouts[2].layout),
                "expected group1/group2 empty layouts to be reused"
            );
            assert_eq!(info.group_layouts[0].hash, info.group_layouts[1].hash);
            assert_eq!(info.group_layouts[1].hash, info.group_layouts[2].hash);

            assert_eq!(info.layout_key.bind_group_layout_hashes.len(), 4);
            for (idx, layout) in info.group_layouts.iter().enumerate() {
                assert_eq!(info.layout_key.bind_group_layout_hashes[idx], layout.hash);
            }
        });
    }

    #[test]
    fn pipeline_bindings_info_does_not_allocate_unused_groups() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            let vs = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];
            let ps = vec![crate::Binding {
                group: 1,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::FRAGMENT,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let info = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(vs.as_slice()),
                    ShaderBindingSet::Guest(ps.as_slice()),
                ],
                BindGroupIndexValidation::GuestShaders,
            )
            .unwrap();

            assert_eq!(
                info.layout_key.bind_group_layout_hashes.len(),
                2,
                "VS/PS pipelines should only allocate the groups they use"
            );
            assert_eq!(info.group_layouts.len(), 2);
            assert_eq!(info.group_bindings.len(), 2);
            assert_eq!(info.group_bindings[0].len(), 1);
            assert_eq!(info.group_bindings[0][0].group, 0);
            assert_eq!(info.group_bindings[1].len(), 1);
            assert_eq!(info.group_bindings[1][0].group, 1);
        });
    }

    #[test]
    fn pipeline_bindings_info_rejects_kind_mismatch_across_shaders() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            let a = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];
            let b = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::FRAGMENT,
                kind: crate::BindingKind::ConstantBuffer {
                    slot: 0,
                    reg_count: 1,
                },
            }];

            let err = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(a.as_slice()),
                    ShaderBindingSet::Guest(b.as_slice()),
                ],
                BindGroupIndexValidation::GuestShaders,
            )
            .unwrap_err()
            .to_string();

            assert!(
                err.contains("kind mismatch across shaders"),
                "unexpected error for kind mismatch: {err}"
            );
            assert!(
                err.contains("Texture2D") && err.contains("ConstantBuffer"),
                "expected error to mention both kinds; got: {err}"
            );
        });
    }

    #[test]
    fn pipeline_bindings_info_rejects_out_of_range_group() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            let bindings = vec![crate::Binding {
                group: MAX_GUEST_BIND_GROUP_INDEX + 1,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let err = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [ShaderBindingSet::Guest(bindings.as_slice())],
                BindGroupIndexValidation::GuestShaders,
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("out of range") && err.contains("binding model"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn pipeline_bindings_info_allows_internal_bind_group_4() {
        pollster::block_on(async {
            const INTERNAL_GROUP: u32 = BIND_GROUP_INTERNAL_EMULATION + 1;
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();
            if device.limits().max_bind_groups <= INTERNAL_GROUP {
                skip_or_panic(
                    module_path!(),
                    "adapter does not support 5 bind groups (required for @group(4))",
                );
                return;
            }

            let guest = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];
            let internal = vec![crate::Binding {
                group: INTERNAL_GROUP,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let info = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(guest.as_slice()),
                    ShaderBindingSet::Internal(internal.as_slice()),
                ],
                BindGroupIndexValidation::GuestAndInternal {
                    max_internal_bind_group_index: INTERNAL_GROUP,
                },
            )
            .unwrap();

            let internal_group_idx = INTERNAL_GROUP as usize;
            assert_eq!(info.group_bindings.len(), internal_group_idx + 1);
            assert_eq!(info.group_bindings[0].len(), 1);
            for g in 1..internal_group_idx {
                assert!(info.group_bindings[g].is_empty());
            }
            assert_eq!(info.group_bindings[internal_group_idx].len(), 1);
            assert_eq!(
                info.group_bindings[internal_group_idx][0].group,
                INTERNAL_GROUP
            );

            assert_eq!(info.group_layouts.len(), internal_group_idx + 1);
            assert_eq!(
                info.layout_key.bind_group_layout_hashes.len(),
                internal_group_idx + 1
            );
            for (hash, layout) in info
                .layout_key
                .bind_group_layout_hashes
                .iter()
                .copied()
                .zip(&info.group_layouts)
            {
                assert_eq!(hash, layout.hash);
            }
        });
    }

    #[test]
    fn pipeline_bindings_info_rejects_group_over_device_limit() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            // `max_bind_groups` is a count; the largest valid group index is `max_bind_groups - 1`.
            // Use `max_bind_groups` itself as a deterministic out-of-range group index.
            let invalid_group = device.limits().max_bind_groups;
            let internal = vec![crate::Binding {
                group: invalid_group,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let err = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [ShaderBindingSet::Internal(internal.as_slice())],
                BindGroupIndexValidation::GuestAndInternal {
                    max_internal_bind_group_index: invalid_group,
                },
            )
            .unwrap_err()
            .to_string();

            assert!(
                err.contains("max_bind_groups"),
                "expected error to mention max_bind_groups, got: {err}"
            );
        });
    }

    #[test]
    fn pipeline_bindings_info_guest_bindings_remain_stage_scoped_with_internal_groups_enabled() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            let internal_only_group = MAX_GUEST_BIND_GROUP_INDEX + 1;
            let guest = vec![crate::Binding {
                group: internal_only_group,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let err = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [ShaderBindingSet::Guest(guest.as_slice())],
                BindGroupIndexValidation::GuestAndInternal {
                    max_internal_bind_group_index: internal_only_group,
                },
            )
            .unwrap_err()
            .to_string();

            assert!(
                err.contains("guest binding") && err.contains("out of range"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn pipeline_bindings_info_rejects_cbuffer_over_device_limit() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            let device = rt.device();
            let mut layout_cache = BindGroupLayoutCache::new();

            let max = device.limits().max_uniform_buffer_binding_size as u64;
            let reg_count = match u32::try_from((max / UNIFORM_BINDING_SIZE_ALIGN) + 1) {
                Ok(v) => v,
                Err(_) => {
                    skip_or_panic(
                        module_path!(),
                        "cannot construct reg_count over device limit",
                    );
                    return;
                }
            };

            let bindings = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_CBUFFER,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::ConstantBuffer { slot: 0, reg_count },
            }];

            let err = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [ShaderBindingSet::Guest(bindings.as_slice())],
                BindGroupIndexValidation::GuestShaders,
            )
            .unwrap_err()
            .to_string();

            assert!(
                err.contains("exceeds device limit"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn gs_group3_constant_buffer_updates_affect_compute_emulation_output() {
        const TEX_W: u32 = 64;
        const TEX_H: u32 = 64;
        const BYTES_PER_ROW: u32 = TEX_W * 4; // 64 * 4 = 256 (COPY_BYTES_PER_ROW_ALIGNMENT)

        pollster::block_on(async {
            let (adapter, device, queue) = match new_device_queue_for_tests().await {
                Ok(v) => v,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            // This test exercises the compute-based GS emulation path, which requires compute and
            // storage-buffer support.
            let downlevel = adapter.get_downlevel_capabilities();
            if !downlevel
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
            {
                skip_or_panic(module_path!(), "adapter does not support compute shaders");
                return;
            }
            if device.limits().max_storage_buffers_per_shader_stage == 0 {
                skip_or_panic(
                    module_path!(),
                    "storage buffers are not supported by this adapter",
                );
                return;
            }
            let required_bind_groups = BIND_GROUP_INTERNAL_EMULATION + 1;
            if device.limits().max_bind_groups < required_bind_groups {
                skip_or_panic(
                    module_path!(),
                    &format!(
                        "adapter does not support {required_bind_groups} bind groups (required by this test)"
                    ),
                );
                return;
            }

            // Build stage-scoped bind group layouts via the same reflection-driven path the D3D11
            // executor uses. The geometry stage is emulated via a compute shader, but its slot
            // space still lives in its own bind group (@group(3)).
            let mut layout_cache = BindGroupLayoutCache::new();
            let vs_bindings = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_CBUFFER,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::ConstantBuffer {
                    slot: 0,
                    reg_count: 1,
                },
            }];
            let gs_bindings = vec![crate::Binding {
                group: 3,
                binding: BINDING_BASE_CBUFFER,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::ConstantBuffer {
                    slot: 0,
                    reg_count: 1,
                },
            }];

            let info = build_pipeline_bindings_info(
                &device,
                &mut layout_cache,
                [
                    ShaderBindingSet::Guest(vs_bindings.as_slice()),
                    ShaderBindingSet::Internal(gs_bindings.as_slice()),
                ],
                BindGroupIndexValidation::GuestAndInternal {
                    max_internal_bind_group_index: BIND_GROUP_INTERNAL_EMULATION,
                },
            )
            .expect("build pipeline bindings info");
            assert_eq!(
                info.group_layouts.len(),
                4,
                "expected group layouts for groups 0..=3"
            );

            // Internal GS emulation buffers share `@group(3)` with the GS stage bindings. They use
            // bindings in the internal range (`BINDING_BASE_INTERNAL..`) so they don't collide with
            // slot-derived D3D11 bindings (`b#`/`t#`/`s#`/`u#`) within the group.
            //
            // Keep this disjoint from the vertex/index pulling helpers, which also consume part of
            // the internal binding-number space.
            let out_vertices_binding: u32 = crate::binding_model::BINDING_BASE_INTERNAL + 64;

            let group3_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("gs emulation group3 bind group layout"),
                entries: &[
                    // GS cbuffer (b0 → @binding(0)).
                    wgpu::BindGroupLayoutEntry {
                        binding: BINDING_BASE_CBUFFER,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(16),
                        },
                        count: None,
                    },
                    // Internal output buffer.
                    wgpu::BindGroupLayoutEntry {
                        binding: out_vertices_binding,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

            let layout_refs: [&wgpu::BindGroupLayout; 4] = [
                info.group_layouts[0].layout.as_ref(),
                info.group_layouts[1].layout.as_ref(),
                info.group_layouts[2].layout.as_ref(),
                &group3_layout,
            ];

            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("gs emulation pipeline layout"),
                bind_group_layouts: &layout_refs,
                push_constant_ranges: &[],
            });

            let compute_wgsl = r#"
struct Cb0 { regs: array<vec4<u32>, 1> };

@group(0) @binding(0) var<uniform> vs_cb0: Cb0;
@group(3) @binding(0) var<uniform> gs_cb0: Cb0;

struct Vertex {
  pos: vec2<f32>,
  // 8 bytes padding so `color` is 16-byte aligned.
  _pad0: vec2<f32>,
  color: vec4<f32>,
};

@group(3) @binding(__OUT_VERTICES_BINDING__) var<storage, read_write> out_vertices: array<Vertex>;

fn base_pos(i: u32) -> vec2<f32> {
  if (i == 0u) { return vec2<f32>(-0.5, -0.5); }
  if (i == 1u) { return vec2<f32>( 0.0,  0.5); }
  return vec2<f32>( 0.5, -0.5);
}

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
  let i = id.x;
  if (i >= 3u) { return; }

  // Read GS constant buffer as raw u32 bits (mirrors `shader_translate` output).
  let off = vec2<f32>(
    bitcast<f32>(gs_cb0.regs[0].x),
    bitcast<f32>(gs_cb0.regs[0].y)
  );

  out_vertices[i].pos = base_pos(i) + off;
  out_vertices[i]._pad0 = vec2<f32>(0.0, 0.0);
  out_vertices[i].color = vec4<f32>(1.0, 0.0, 0.0, 1.0);

  // Touch the VS cbuffer so the binding is considered live (it is otherwise unused here).
  _ = vs_cb0.regs[0].x;
 }
 "#;
            let compute_wgsl = compute_wgsl.replace(
                "__OUT_VERTICES_BINDING__",
                &out_vertices_binding.to_string(),
            );

            let compute_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("gs emulation compute module"),
                source: wgpu::ShaderSource::Wgsl(compute_wgsl.into()),
            });
            let compute_pipeline =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("gs emulation compute pipeline"),
                    layout: Some(&pipeline_layout),
                    module: &compute_module,
                    entry_point: "cs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                });

            let render_wgsl = r#"
struct VsIn {
  @location(0) pos: vec2<f32>,
  @location(1) color: vec4<f32>,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
  var out: VsOut;
  out.pos = vec4<f32>(input.pos, 0.0, 1.0);
  out.color = input.color;
  return out;
}

@fragment
fn fs_main(@location(0) color: vec4<f32>) -> @location(0) vec4<f32> {
  return color;
}
"#;
            let render_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("gs emulation render module"),
                source: wgpu::ShaderSource::Wgsl(render_wgsl.into()),
            });
            let render_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("gs emulation render pipeline layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });
            let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("gs emulation render pipeline"),
                layout: Some(&render_layout),
                vertex: wgpu::VertexState {
                    module: &render_module,
                    entry_point: "vs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: 32,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 0,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 16,
                                shader_location: 1,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &render_module,
                    entry_point: "fs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

            let vs_cb0_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gs emulation vs cb0"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let gs_cb0_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gs emulation gs cb0"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vs_cb0_buffer, 0, &[0u8; 16]);
            queue.write_buffer(&gs_cb0_buffer, 0, &[0u8; 16]);

            // The executor normally provides these dummy resources for unbound slots.
            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gs emulation dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gs emulation dummy storage"),
                size: 256,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("gs emulation dummy texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_texture_view_2d =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let dummy_texture_view_2d_array =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });
            let mut sampler_cache = SamplerCache::new();
            let default_sampler = sampler_cache.get_or_create(
                &device,
                &wgpu::SamplerDescriptor {
                    label: Some("gs emulation default sampler"),
                    address_mode_u: wgpu::AddressMode::ClampToEdge,
                    address_mode_v: wgpu::AddressMode::ClampToEdge,
                    address_mode_w: wgpu::AddressMode::ClampToEdge,
                    mag_filter: wgpu::FilterMode::Nearest,
                    min_filter: wgpu::FilterMode::Nearest,
                    mipmap_filter: wgpu::FilterMode::Nearest,
                    ..Default::default()
                },
            );

            struct UniformProvider<'a> {
                id: BufferId,
                buffer: &'a wgpu::Buffer,
                dummy_uniform: &'a wgpu::Buffer,
                dummy_storage: &'a wgpu::Buffer,
                dummy_texture_view_2d: &'a wgpu::TextureView,
                dummy_texture_view_2d_array: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for UniformProvider<'_> {
                fn constant_buffer(&self, slot: u32) -> Option<BufferBinding<'_>> {
                    if slot != 0 {
                        return None;
                    }
                    Some(BufferBinding {
                        id: self.id,
                        buffer: self.buffer,
                        offset: 0,
                        size: None,
                        total_size: 16,
                    })
                }

                fn constant_buffer_scratch(&self, _slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
                    None
                }

                fn texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn texture2d_array(
                    &self,
                    _slot: u32,
                ) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_storage(&self) -> &wgpu::Buffer {
                    self.dummy_storage
                }

                fn dummy_storage_texture_view(
                    &self,
                    _format: crate::StorageTextureFormat,
                ) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d_array
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let vs_provider = UniformProvider {
                id: BufferId(1),
                buffer: &vs_cb0_buffer,
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view_2d,
                dummy_texture_view_2d_array: &dummy_texture_view_2d_array,
                default_sampler: &default_sampler,
            };

            let mut bind_group_cache = BindGroupCache::new(32);
            let bg0 = build_bind_group(
                &device,
                &mut bind_group_cache,
                &info.group_layouts[0],
                &info.group_bindings[0],
                &vs_provider,
            )
            .expect("build group0 bind group");

            // Empty groups still appear in the pipeline layout; create empty bind groups so we can
            // bind every group index consistently.
            let empty_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("gs emulation empty bind group"),
                layout: info
                    .group_layouts
                    .get(1)
                    .expect("group1 layout exists")
                    .layout
                    .as_ref(),
                entries: &[],
            });

            let out_vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gs emulation vertices"),
                size: 32 * 3,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX,
                mapped_at_creation: false,
            });
            let bg3 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("gs emulation group3 bind group"),
                layout: &group3_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: BINDING_BASE_CBUFFER,
                        resource: gs_cb0_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: out_vertices_binding,
                        resource: out_vertices.as_entire_binding(),
                    },
                ],
            });

            let output_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("gs emulation output texture"),
                size: wgpu::Extent3d {
                    width: TEX_W,
                    height: TEX_H,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

            async fn readback_rgba8(
                device: &wgpu::Device,
                queue: &wgpu::Queue,
                texture: &wgpu::Texture,
            ) -> Vec<u8> {
                let staging = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gs emulation readback buffer"),
                    size: (BYTES_PER_ROW as u64) * (TEX_H as u64),
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });

                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("gs emulation readback encoder"),
                });
                encoder.copy_texture_to_buffer(
                    wgpu::ImageCopyTexture {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyBuffer {
                        buffer: &staging,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(BYTES_PER_ROW),
                            rows_per_image: Some(TEX_H),
                        },
                    },
                    wgpu::Extent3d {
                        width: TEX_W,
                        height: TEX_H,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit([encoder.finish()]);

                let slice = staging.slice(..);
                let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
                slice.map_async(wgpu::MapMode::Read, move |v| {
                    sender.send(v).ok();
                });
                #[cfg(not(target_arch = "wasm32"))]
                device.poll(wgpu::Maintain::Wait);
                #[cfg(target_arch = "wasm32")]
                device.poll(wgpu::Maintain::Poll);
                receiver.receive().await.unwrap().unwrap();

                let data = slice.get_mapped_range().to_vec();
                staging.unmap();
                data
            }

            #[allow(clippy::too_many_arguments)]
            async fn draw_and_sample(
                device: &wgpu::Device,
                queue: &wgpu::Queue,
                gs_cb0_buffer: &wgpu::Buffer,
                compute_pipeline: &wgpu::ComputePipeline,
                bg0: &wgpu::BindGroup,
                bg3: &wgpu::BindGroup,
                empty_bg: &wgpu::BindGroup,
                render_pipeline: &wgpu::RenderPipeline,
                out_vertices: &wgpu::Buffer,
                output_tex: &wgpu::Texture,
                output_view: &wgpu::TextureView,
                offset_x: f32,
            ) -> [u8; 4] {
                // Write GS offset into cb0[0].xy.
                let mut bytes = [0u8; 16];
                bytes[0..4].copy_from_slice(&offset_x.to_bits().to_le_bytes());
                bytes[4..8].copy_from_slice(&0f32.to_bits().to_le_bytes());
                queue.write_buffer(gs_cb0_buffer, 0, &bytes);

                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("gs emulation encoder"),
                });

                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("gs emulation compute pass"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(compute_pipeline);
                    pass.set_bind_group(0, bg0, &[]);
                    pass.set_bind_group(1, empty_bg, &[]);
                    pass.set_bind_group(2, empty_bg, &[]);
                    pass.set_bind_group(3, bg3, &[]);
                    pass.dispatch_workgroups(3, 1, 1);
                }

                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("gs emulation render pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: output_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    pass.set_pipeline(render_pipeline);
                    pass.set_vertex_buffer(0, out_vertices.slice(..));
                    pass.draw(0..3, 0..1);
                }

                queue.submit([encoder.finish()]);

                let data = readback_rgba8(device, queue, output_tex).await;
                let x = (TEX_W / 2) as usize;
                let y = (TEX_H / 2) as usize;
                let idx = y * (BYTES_PER_ROW as usize) + x * 4;
                [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]]
            }

            let center_a = draw_and_sample(
                &device,
                &queue,
                &gs_cb0_buffer,
                &compute_pipeline,
                bg0.as_ref(),
                &bg3,
                &empty_bg,
                &render_pipeline,
                &out_vertices,
                &output_tex,
                &output_view,
                0.0,
            )
            .await;
            let center_b = draw_and_sample(
                &device,
                &queue,
                &gs_cb0_buffer,
                &compute_pipeline,
                bg0.as_ref(),
                &bg3,
                &empty_bg,
                &render_pipeline,
                &out_vertices,
                &output_tex,
                &output_view,
                2.0,
            )
            .await;

            assert!(
                center_a[0] > 200 && center_a[1] < 50 && center_a[2] < 50,
                "expected first draw to be red-ish, got {center_a:?}"
            );
            assert!(
                center_b[0] < 50 && center_b[1] < 50 && center_b[2] < 50,
                "expected second draw to be black-ish after cbuffer update, got {center_b:?}"
            );
        });
    }

    #[test]
    fn build_bind_group_falls_back_to_dummy_for_too_small_constant_buffer() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            let device = rt.device();
            if device.limits().max_storage_buffers_per_shader_stage == 0 {
                skip_or_panic(
                    module_path!(),
                    "storage buffers are not supported by this adapter",
                );
                return;
            }
            if !device
                .features()
                .contains(wgpu::Features::VERTEX_WRITABLE_STORAGE)
            {
                skip_or_panic(
                    module_path!(),
                    "storage buffers are not supported in the vertex stage on this adapter",
                );
                return;
            }

            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy storage"),
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let too_small_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test too-small uniform"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });

            let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("reflection_bindings test dummy texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_texture_view_2d =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let dummy_texture_view_2d_array =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });

            let mut sampler_cache = SamplerCache::new();
            let default_sampler =
                sampler_cache.get_or_create(device, &wgpu::SamplerDescriptor::default());

            let binding = crate::Binding {
                group: 0,
                binding: BINDING_BASE_CBUFFER,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::ConstantBuffer {
                    slot: 0,
                    reg_count: 4, // 64 bytes required
                },
            };

            let layout_entry = binding_to_layout_entry(&binding).unwrap();
            let mut layout_cache = BindGroupLayoutCache::new();
            let layout = layout_cache.get_or_create(device, &[layout_entry]);

            struct TestProvider<'a> {
                cb: Option<(BufferId, &'a wgpu::Buffer, u64, Option<u64>, u64)>,
                dummy_uniform: &'a wgpu::Buffer,
                dummy_storage: &'a wgpu::Buffer,
                dummy_texture_view_2d: &'a wgpu::TextureView,
                dummy_texture_view_2d_array: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for TestProvider<'_> {
                fn constant_buffer(&self, slot: u32) -> Option<BufferBinding<'_>> {
                    if slot != 0 {
                        return None;
                    }
                    self.cb
                        .map(|(id, buffer, offset, size, total_size)| BufferBinding {
                            id,
                            buffer,
                            offset,
                            size,
                            total_size,
                        })
                }

                fn constant_buffer_scratch(&self, _slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
                    None
                }

                fn texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn texture2d_array(
                    &self,
                    _slot: u32,
                ) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_storage(&self) -> &wgpu::Buffer {
                    self.dummy_storage
                }

                fn dummy_storage_texture_view(
                    &self,
                    _format: crate::StorageTextureFormat,
                ) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d_array
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider_too_small = TestProvider {
                cb: Some((BufferId(1), &too_small_uniform, 0, None, 32)),
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view_2d,
                dummy_texture_view_2d_array: &dummy_texture_view_2d_array,
                default_sampler: &default_sampler,
            };
            let provider_none = TestProvider {
                cb: None,
                ..provider_too_small
            };

            let mut bind_group_cache = BindGroupCache::new(32);
            let bg_fallback = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider_too_small,
            )
            .unwrap();
            let bg_dummy = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider_none,
            )
            .unwrap();

            assert!(
                Arc::ptr_eq(&bg_fallback, &bg_dummy),
                "expected too-small constant buffer binding to fall back to the dummy binding"
            );
        });
    }

    #[test]
    fn build_bind_group_falls_back_to_dummy_for_missing_srv_uav_buffers() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            let device = rt.device();
            if device.limits().max_storage_buffers_per_shader_stage == 0 {
                skip_or_panic(
                    module_path!(),
                    "storage buffers are not supported by this adapter",
                );
                return;
            }
            if !device
                .features()
                .contains(wgpu::Features::VERTEX_WRITABLE_STORAGE)
            {
                skip_or_panic(
                    module_path!(),
                    "storage buffers are not supported in the vertex stage on this adapter",
                );
                return;
            }

            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy storage"),
                size: 256,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });

            let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("reflection_bindings test dummy texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_texture_view_2d =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let dummy_texture_view_2d_array =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });

            let mut sampler_cache = SamplerCache::new();
            let default_sampler =
                sampler_cache.get_or_create(device, &wgpu::SamplerDescriptor::default());

            let srv = crate::Binding {
                group: 0,
                binding: BINDING_BASE_TEXTURE,
                // Using storage buffers in the vertex stage requires additional wgpu features on
                // many backends (e.g. VERTEX_WRITABLE_STORAGE). This test is only validating the
                // dummy-buffer fallback behavior, so keep visibility to a stage that works on
                // downlevel adapters without extra features.
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::SrvBuffer { slot: 0 },
            };
            let uav = crate::Binding {
                group: 0,
                binding: BINDING_BASE_UAV,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::UavBuffer { slot: 0 },
            };

            let entries = [
                binding_to_layout_entry(&srv).unwrap(),
                binding_to_layout_entry(&uav).unwrap(),
            ];
            let mut layout_cache = BindGroupLayoutCache::new();
            let layout = layout_cache.get_or_create(device, &entries);

            struct DummyProvider<'a> {
                dummy_uniform: &'a wgpu::Buffer,
                dummy_storage: &'a wgpu::Buffer,
                dummy_texture_view_2d: &'a wgpu::TextureView,
                dummy_texture_view_2d_array: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for DummyProvider<'_> {
                fn constant_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
                    None
                }

                fn constant_buffer_scratch(&self, _slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
                    None
                }

                fn texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn texture2d_array(
                    &self,
                    _slot: u32,
                ) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_storage(&self) -> &wgpu::Buffer {
                    self.dummy_storage
                }

                fn dummy_storage_texture_view(
                    &self,
                    _format: crate::StorageTextureFormat,
                ) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d_array
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider = DummyProvider {
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view_2d,
                dummy_texture_view_2d_array: &dummy_texture_view_2d_array,
                default_sampler: &default_sampler,
            };

            let mut bind_group_cache = BindGroupCache::new(16);
            build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                &[srv, uav],
                &provider,
            )
            .expect("bind group should build with dummy storage-buffer fallback");
        });
    }

    #[test]
    fn build_bind_group_uses_scratch_for_unaligned_uniform_offsets() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            let device = rt.device();
            let uniform_align = device.limits().min_uniform_buffer_offset_alignment as u64;
            // WebGPU spec requires this to be at least 256, but keep the test robust in case wgpu
            // reports a smaller value on some backends.
            let offset = 4u64;
            if uniform_align <= 1 || offset.is_multiple_of(uniform_align) {
                skip_or_panic(
                    module_path!(),
                    &format!("cannot pick unaligned offset for uniform alignment {uniform_align}"),
                );
                return;
            }

            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let real_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test real uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let scratch_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test scratch uniform"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy storage"),
                size: 4,
                usage: wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("reflection_bindings test dummy texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_texture_view_2d =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let dummy_texture_view_2d_array =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });

            let mut sampler_cache = SamplerCache::new();
            let default_sampler =
                sampler_cache.get_or_create(device, &wgpu::SamplerDescriptor::default());

            let binding = crate::Binding {
                group: 0,
                binding: BINDING_BASE_CBUFFER,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::ConstantBuffer {
                    slot: 0,
                    reg_count: 4, // 64 bytes minimum
                },
            };
            let layout_entry = binding_to_layout_entry(&binding).unwrap();
            let mut layout_cache = BindGroupLayoutCache::new();
            let layout = layout_cache.get_or_create(device, &[layout_entry]);

            #[derive(Clone, Copy)]
            struct TestProvider<'a> {
                buffer_id: BufferId,
                buffer: &'a wgpu::Buffer,
                offset: u64,
                size: Option<u64>,
                total_size: u64,
                scratch: Option<(BufferId, &'a wgpu::Buffer)>,
                dummy_uniform: &'a wgpu::Buffer,
                dummy_storage: &'a wgpu::Buffer,
                dummy_texture_view_2d: &'a wgpu::TextureView,
                dummy_texture_view_2d_array: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for TestProvider<'_> {
                fn constant_buffer(&self, slot: u32) -> Option<BufferBinding<'_>> {
                    if slot != 0 {
                        return None;
                    }
                    Some(BufferBinding {
                        id: self.buffer_id,
                        buffer: self.buffer,
                        offset: self.offset,
                        size: self.size,
                        total_size: self.total_size,
                    })
                }

                fn constant_buffer_scratch(&self, slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
                    if slot != 0 {
                        return None;
                    }
                    self.scratch
                }

                fn texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn texture2d_array(
                    &self,
                    _slot: u32,
                ) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_storage(&self) -> &wgpu::Buffer {
                    self.dummy_storage
                }

                fn dummy_storage_texture_view(
                    &self,
                    _format: crate::StorageTextureFormat,
                ) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d_array
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider_with_scratch = TestProvider {
                buffer_id: BufferId(1),
                buffer: &real_uniform,
                offset,
                size: Some(64),
                total_size: 256,
                scratch: Some((BufferId(2), &scratch_uniform)),
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view_2d,
                dummy_texture_view_2d_array: &dummy_texture_view_2d_array,
                default_sampler: &default_sampler,
            };
            let provider_without_scratch = TestProvider {
                scratch: None,
                ..provider_with_scratch
            };

            let mut bind_group_cache = BindGroupCache::new(32);
            let bg_scratch = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider_with_scratch,
            )
            .unwrap();
            let bg_dummy = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider_without_scratch,
            )
            .unwrap();

            assert!(
                !Arc::ptr_eq(&bg_scratch, &bg_dummy),
                "expected scratch-backed and dummy-backed bind groups to differ"
            );
        });
    }

    #[test]
    fn build_bind_group_falls_back_to_dummy_for_unaligned_storage_offsets() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            let device = rt.device();
            if device.limits().max_storage_buffers_per_shader_stage == 0 {
                skip_or_panic(
                    module_path!(),
                    "storage buffers are not supported by this adapter",
                );
                return;
            }
            let storage_align = device.limits().min_storage_buffer_offset_alignment as u64;
            let offset = 4u64;
            if storage_align <= 1 || offset.is_multiple_of(storage_align) {
                skip_or_panic(
                    module_path!(),
                    &format!("cannot pick unaligned offset for storage alignment {storage_align}"),
                );
                return;
            }

            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy storage"),
                size: 4,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            let real_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test real storage"),
                size: 256,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });

            let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("reflection_bindings test dummy texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_texture_view_2d =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let dummy_texture_view_2d_array =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });

            let mut sampler_cache = SamplerCache::new();
            let default_sampler =
                sampler_cache.get_or_create(device, &wgpu::SamplerDescriptor::default());

            let binding = crate::Binding {
                group: 0,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::SrvBuffer { slot: 0 },
            };
            let layout_entry = binding_to_layout_entry(&binding).unwrap();
            let mut layout_cache = BindGroupLayoutCache::new();
            let layout = layout_cache.get_or_create(device, &[layout_entry]);

            struct TestProvider<'a> {
                buffer_id: BufferId,
                buffer: Option<&'a wgpu::Buffer>,
                offset: u64,
                size: Option<u64>,
                total_size: u64,
                dummy_uniform: &'a wgpu::Buffer,
                dummy_storage: &'a wgpu::Buffer,
                dummy_texture_view_2d: &'a wgpu::TextureView,
                dummy_texture_view_2d_array: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for TestProvider<'_> {
                fn constant_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
                    None
                }

                fn constant_buffer_scratch(&self, _slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
                    None
                }

                fn srv_buffer(&self, slot: u32) -> Option<BufferBinding<'_>> {
                    if slot != 0 {
                        return None;
                    }
                    let buffer = self.buffer?;
                    Some(BufferBinding {
                        id: self.buffer_id,
                        buffer,
                        offset: self.offset,
                        size: self.size,
                        total_size: self.total_size,
                    })
                }

                fn uav_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
                    None
                }

                fn texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn texture2d_array(
                    &self,
                    _slot: u32,
                ) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_storage(&self) -> &wgpu::Buffer {
                    self.dummy_storage
                }

                fn dummy_storage_texture_view(
                    &self,
                    _format: crate::StorageTextureFormat,
                ) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d_array
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider_misaligned = TestProvider {
                buffer_id: BufferId(1),
                buffer: Some(&real_storage),
                offset,
                size: Some(64),
                total_size: 256,
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view_2d,
                dummy_texture_view_2d_array: &dummy_texture_view_2d_array,
                default_sampler: &default_sampler,
            };
            let provider_dummy = TestProvider {
                buffer_id: BufferId(1),
                buffer: None,
                offset: 0,
                size: None,
                total_size: 0,
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view_2d,
                dummy_texture_view_2d_array: &dummy_texture_view_2d_array,
                default_sampler: &default_sampler,
            };

            let mut bind_group_cache = BindGroupCache::new(32);
            let bg_fallback = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider_misaligned,
            )
            .unwrap();
            let bg_dummy = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider_dummy,
            )
            .unwrap();

            assert!(
                Arc::ptr_eq(&bg_fallback, &bg_dummy),
                "expected unaligned storage buffer binding to fall back to the dummy binding"
            );
        });
    }

    #[test]
    fn clamp_storage_buffer_slice_clamps_to_max_storage_binding_size() {
        let mut limits = wgpu::Limits::downlevel_defaults();
        limits.max_storage_buffer_binding_size = 64;
        limits.min_storage_buffer_offset_alignment = 256;

        let (_offset, size) = clamp_storage_buffer_slice(
            0,
            Some(1024),
            2048,
            limits.min_storage_buffer_offset_alignment as u64,
            limits.max_storage_buffer_binding_size as u64,
        )
        .expect("expected oversized slice to be clamped, not rejected");

        assert_eq!(size, 64);
    }

    #[test]
    fn binding_to_layout_entry_rejects_slot_binding_mismatch() {
        let cb = crate::Binding {
            group: 0,
            binding: BINDING_BASE_CBUFFER + 1,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::ConstantBuffer {
                slot: 0,
                reg_count: 1,
            },
        };
        let err = binding_to_layout_entry(&cb).unwrap_err().to_string();
        assert!(
            err.contains("expected @binding("),
            "unexpected error for cbuffer binding mismatch: {err}"
        );

        let tex = crate::Binding {
            group: 0,
            binding: BINDING_BASE_TEXTURE + 1,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::Texture2D { slot: 0 },
        };
        let err = binding_to_layout_entry(&tex).unwrap_err().to_string();
        assert!(
            err.contains("expected @binding("),
            "unexpected error for texture binding mismatch: {err}"
        );

        let srv_buf = crate::Binding {
            group: 0,
            binding: BINDING_BASE_TEXTURE + 1,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::SrvBuffer { slot: 0 },
        };
        let err = binding_to_layout_entry(&srv_buf).unwrap_err().to_string();
        assert!(
            err.contains("expected @binding("),
            "unexpected error for srv buffer binding mismatch: {err}"
        );

        let sampler = crate::Binding {
            group: 0,
            binding: BINDING_BASE_SAMPLER + 1,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::Sampler { slot: 0 },
        };
        let err = binding_to_layout_entry(&sampler).unwrap_err().to_string();
        assert!(
            err.contains("expected @binding("),
            "unexpected error for sampler binding mismatch: {err}"
        );

        let uav_buf = crate::Binding {
            group: 0,
            binding: BINDING_BASE_UAV + 1,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::UavBuffer { slot: 0 },
        };
        let err = binding_to_layout_entry(&uav_buf).unwrap_err().to_string();
        assert!(
            err.contains("expected @binding("),
            "unexpected error for uav buffer binding mismatch: {err}"
        );
    }

    #[test]
    fn binding_to_layout_entry_rejects_out_of_range_slots() {
        let cb = crate::Binding {
            group: 0,
            binding: BINDING_BASE_CBUFFER + MAX_CBUFFER_SLOTS,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::ConstantBuffer {
                slot: MAX_CBUFFER_SLOTS,
                reg_count: 1,
            },
        };
        let err = binding_to_layout_entry(&cb).unwrap_err().to_string();
        assert!(
            err.contains("out of range"),
            "unexpected error for out-of-range cbuffer: {err}"
        );

        let tex = crate::Binding {
            group: 0,
            binding: BINDING_BASE_TEXTURE + MAX_TEXTURE_SLOTS,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::Texture2D {
                slot: MAX_TEXTURE_SLOTS,
            },
        };
        let err = binding_to_layout_entry(&tex).unwrap_err().to_string();
        assert!(
            err.contains("out of range"),
            "unexpected error for out-of-range texture: {err}"
        );

        let srv_buf = crate::Binding {
            group: 0,
            binding: BINDING_BASE_TEXTURE + MAX_TEXTURE_SLOTS,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::SrvBuffer {
                slot: MAX_TEXTURE_SLOTS,
            },
        };
        let err = binding_to_layout_entry(&srv_buf).unwrap_err().to_string();
        assert!(
            err.contains("out of range"),
            "unexpected error for out-of-range srv buffer: {err}"
        );

        let sampler = crate::Binding {
            group: 0,
            binding: BINDING_BASE_SAMPLER + MAX_SAMPLER_SLOTS,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::Sampler {
                slot: MAX_SAMPLER_SLOTS,
            },
        };
        let err = binding_to_layout_entry(&sampler).unwrap_err().to_string();
        assert!(
            err.contains("out of range"),
            "unexpected error for out-of-range sampler: {err}"
        );

        let uav_buf = crate::Binding {
            group: 0,
            binding: BINDING_BASE_UAV + MAX_UAV_SLOTS,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::UavBuffer {
                slot: MAX_UAV_SLOTS,
            },
        };
        let err = binding_to_layout_entry(&uav_buf).unwrap_err().to_string();
        assert!(
            err.contains("out of range"),
            "unexpected error for out-of-range uav buffer: {err}"
        );
    }

    #[test]
    fn binding_to_layout_entry_storage_texture_mapping() {
        let binding = crate::Binding {
            group: 2,
            binding: BINDING_BASE_UAV,
            visibility: wgpu::ShaderStages::COMPUTE,
            kind: crate::BindingKind::UavTexture2DWriteOnly {
                slot: 0,
                format: crate::StorageTextureFormat::Rgba8Unorm,
            },
        };

        let entry = binding_to_layout_entry(&binding).expect("storage texture entry");
        match entry.ty {
            wgpu::BindingType::StorageTexture {
                access,
                format,
                view_dimension,
            } => {
                assert_eq!(access, wgpu::StorageTextureAccess::WriteOnly);
                assert_eq!(format, wgpu::TextureFormat::Rgba8Unorm);
                assert_eq!(view_dimension, wgpu::TextureViewDimension::D2);
            }
            other => panic!("expected storage texture binding type, got {other:?}"),
        }
    }

    #[test]
    fn binding_to_layout_entry_rejects_out_of_range_uav_texture() {
        let uav = crate::Binding {
            group: 0,
            binding: BINDING_BASE_UAV + MAX_UAV_SLOTS,
            visibility: wgpu::ShaderStages::COMPUTE,
            kind: crate::BindingKind::UavTexture2DWriteOnly {
                slot: MAX_UAV_SLOTS,
                format: crate::StorageTextureFormat::Rgba8Unorm,
            },
        };
        let err = binding_to_layout_entry(&uav).unwrap_err().to_string();
        assert!(
            err.contains("out of range"),
            "unexpected error for out-of-range uav texture: {err}"
        );
    }

    #[test]
    fn binding_to_layout_entry_rejects_mismatched_binding_numbers() {
        let cb = crate::Binding {
            group: 0,
            binding: 1,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::ConstantBuffer {
                slot: 0,
                reg_count: 1,
            },
        };
        assert!(binding_to_layout_entry(&cb)
            .unwrap_err()
            .to_string()
            .contains("cbuffer slot 0 expected @binding(0)"));

        let tex = crate::Binding {
            group: 0,
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::Texture2D { slot: 0 },
        };
        assert!(binding_to_layout_entry(&tex)
            .unwrap_err()
            .to_string()
            .contains("texture slot 0 expected @binding(32)"));

        let srv_buf = crate::Binding {
            group: 0,
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::SrvBuffer { slot: 0 },
        };
        assert!(binding_to_layout_entry(&srv_buf)
            .unwrap_err()
            .to_string()
            .contains("srv buffer slot 0 expected @binding(32)"));

        let sampler = crate::Binding {
            group: 0,
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::Sampler { slot: 0 },
        };
        assert!(binding_to_layout_entry(&sampler)
            .unwrap_err()
            .to_string()
            .contains("sampler slot 0 expected @binding(160)"));

        let uav_buf = crate::Binding {
            group: 0,
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            kind: crate::BindingKind::UavBuffer { slot: 0 },
        };
        assert!(binding_to_layout_entry(&uav_buf)
            .unwrap_err()
            .to_string()
            .contains(&format!(
                "uav buffer slot 0 expected @binding({})",
                BINDING_BASE_UAV
            )));
    }

    #[test]
    fn binding_to_layout_entry_uav_buffer_is_storage_and_validated() {
        let ok = crate::Binding {
            group: 0,
            binding: BINDING_BASE_UAV,
            visibility: wgpu::ShaderStages::COMPUTE,
            kind: crate::BindingKind::UavBuffer { slot: 0 },
        };
        let entry = binding_to_layout_entry(&ok).expect("uav buffer layout entry");
        match entry.ty {
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset,
                min_binding_size,
            } => {
                assert!(!read_only, "uav buffer should be read-write storage");
                assert!(!has_dynamic_offset);
                assert!(min_binding_size.is_none());
            }
            other => panic!("expected storage buffer binding type, got {other:?}"),
        }

        let bad_binding_number = crate::Binding {
            binding: BINDING_BASE_UAV + 1,
            ..ok
        };
        let err = binding_to_layout_entry(&bad_binding_number)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("expected @binding("),
            "expected binding number validation error, got: {err}"
        );
    }

    #[test]
    fn build_bind_group_supports_uav_buffer_fallback() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            let device = rt.device();

            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy storage"),
                size: 256,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });

            let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("reflection_bindings test dummy texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_texture_view_2d =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let dummy_texture_view_2d_array =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });

            let mut sampler_cache = SamplerCache::new();
            let default_sampler =
                sampler_cache.get_or_create(device, &wgpu::SamplerDescriptor::default());

            let binding = crate::Binding {
                group: 0,
                binding: BINDING_BASE_UAV,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::UavBuffer { slot: 0 },
            };
            let layout_entry = binding_to_layout_entry(&binding).unwrap();
            let mut layout_cache = BindGroupLayoutCache::new();
            let layout = layout_cache.get_or_create(device, &[layout_entry]);

            struct Provider<'a> {
                dummy_uniform: &'a wgpu::Buffer,
                dummy_storage: &'a wgpu::Buffer,
                dummy_texture_view_2d: &'a wgpu::TextureView,
                dummy_texture_view_2d_array: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for Provider<'_> {
                fn constant_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
                    None
                }

                fn constant_buffer_scratch(&self, _slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
                    None
                }

                fn texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn texture2d_array(
                    &self,
                    _slot: u32,
                ) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_storage(&self) -> &wgpu::Buffer {
                    self.dummy_storage
                }

                fn dummy_storage_texture_view(
                    &self,
                    _format: crate::StorageTextureFormat,
                ) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d_array
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider = Provider {
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view_2d,
                dummy_texture_view_2d_array: &dummy_texture_view_2d_array,
                default_sampler: &default_sampler,
            };

            let mut bind_group_cache = BindGroupCache::new(8);
            let bg1 = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider,
            )
            .expect("uav buffer fallback bind group should build");
            let bg2 = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider,
            )
            .expect("uav buffer fallback bind group should be cached");

            assert!(Arc::ptr_eq(&bg1, &bg2));
        });
    }

    #[test]
    fn build_bind_group_uses_dummy_storage_texture_for_unbound_uav_texture() {
        pollster::block_on(async {
            let (_adapter, device, _queue) = match new_device_queue_for_tests().await {
                Ok(v) => v,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
            if device.limits().max_storage_textures_per_shader_stage == 0 {
                skip_or_panic(
                    module_path!(),
                    "storage textures are not supported by this adapter",
                );
                return;
            }

            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            // The dummy storage buffer isn't used by this test, but the provider trait requires it.
            let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy storage"),
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            // Sampled-texture dummy (TEXTURE_BINDING only).
            let sampled_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("reflection_bindings test dummy sampled texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_texture_view =
                sampled_tex.create_view(&wgpu::TextureViewDescriptor::default());

            // Storage-texture dummy (STORAGE_BINDING only).
            let storage_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("reflection_bindings test dummy storage texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            let dummy_storage_view =
                storage_tex.create_view(&wgpu::TextureViewDescriptor::default());

            let mut sampler_cache = SamplerCache::new();
            let default_sampler =
                sampler_cache.get_or_create(&device, &wgpu::SamplerDescriptor::default());

            struct Provider<'a> {
                dummy_uniform: &'a wgpu::Buffer,
                dummy_storage: &'a wgpu::Buffer,
                dummy_texture_view_2d: &'a wgpu::TextureView,
                dummy_storage_view: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for Provider<'_> {
                fn constant_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
                    None
                }

                fn constant_buffer_scratch(&self, _slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
                    None
                }

                fn texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn texture2d_array(
                    &self,
                    _slot: u32,
                ) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_storage(&self) -> &wgpu::Buffer {
                    self.dummy_storage
                }

                fn dummy_storage_texture_view(
                    &self,
                    format: crate::StorageTextureFormat,
                ) -> &wgpu::TextureView {
                    assert_eq!(
                        format,
                        crate::StorageTextureFormat::Rgba8Unorm,
                        "unexpected format for test"
                    );
                    self.dummy_storage_view
                }

                fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider = Provider {
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view,
                dummy_storage_view: &dummy_storage_view,
                default_sampler: &default_sampler,
            };

            let binding = crate::Binding {
                group: 0,
                binding: BINDING_BASE_UAV,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: crate::BindingKind::UavTexture2DWriteOnly {
                    slot: 0,
                    format: crate::StorageTextureFormat::Rgba8Unorm,
                },
            };
            let layout_entry = binding_to_layout_entry(&binding).expect("layout entry");
            let mut layout_cache = BindGroupLayoutCache::new();
            let layout = layout_cache.get_or_create(&device, &[layout_entry]);

            let mut bind_group_cache = BindGroupCache::new(8);
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            build_bind_group(
                &device,
                &mut bind_group_cache,
                &layout,
                std::slice::from_ref(&binding),
                &provider,
            )
            .expect("bind group should build");
            #[cfg(not(target_arch = "wasm32"))]
            device.poll(wgpu::Maintain::Wait);
            #[cfg(target_arch = "wasm32")]
            device.poll(wgpu::Maintain::Poll);

            let err = device.pop_error_scope().await;
            assert!(err.is_none(), "unexpected wgpu validation error: {err:?}");
        });
    }

    #[test]
    fn pipeline_bindings_merge_and_bind_group_caching() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            let device = rt.device();
            let mut bind_group_layout_cache = BindGroupLayoutCache::new();

            let bindings_a = vec![
                crate::Binding {
                    group: 0,
                    binding: BINDING_BASE_CBUFFER,
                    visibility: wgpu::ShaderStages::VERTEX,
                    kind: crate::BindingKind::ConstantBuffer {
                        slot: 0,
                        reg_count: 4,
                    },
                },
                crate::Binding {
                    group: 0,
                    binding: BINDING_BASE_TEXTURE,
                    visibility: wgpu::ShaderStages::VERTEX,
                    kind: crate::BindingKind::Texture2D { slot: 0 },
                },
                crate::Binding {
                    group: 0,
                    binding: BINDING_BASE_SAMPLER,
                    visibility: wgpu::ShaderStages::VERTEX,
                    kind: crate::BindingKind::Sampler { slot: 0 },
                },
            ];
            let bindings_b = vec![crate::Binding {
                group: 0,
                binding: BINDING_BASE_CBUFFER,
                visibility: wgpu::ShaderStages::FRAGMENT,
                kind: crate::BindingKind::ConstantBuffer {
                    slot: 0,
                    reg_count: 4,
                },
            }];

            let info = build_pipeline_bindings_info(
                device,
                &mut bind_group_layout_cache,
                [
                    ShaderBindingSet::Guest(bindings_a.as_slice()),
                    ShaderBindingSet::Guest(bindings_b.as_slice()),
                ],
                BindGroupIndexValidation::GuestShaders,
            )
            .expect("bindings should merge");
            assert_eq!(info.group_layouts.len(), 1);
            assert_eq!(info.group_bindings.len(), 1);

            let merged_cb = info.group_bindings[0]
                .iter()
                .find(|b| matches!(b.kind, crate::BindingKind::ConstantBuffer { slot: 0, .. }))
                .expect("merged constant buffer binding");
            assert_eq!(
                merged_cb.visibility,
                wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT
            );

            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy storage"),
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("reflection_bindings test dummy texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_texture_view_2d =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let dummy_texture_view_2d_array =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });

            let mut sampler_cache = SamplerCache::new();
            let default_sampler =
                sampler_cache.get_or_create(device, &wgpu::SamplerDescriptor::default());

            struct DummyProvider<'a> {
                dummy_uniform: &'a wgpu::Buffer,
                dummy_storage: &'a wgpu::Buffer,
                dummy_texture_view_2d: &'a wgpu::TextureView,
                dummy_texture_view_2d_array: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for DummyProvider<'_> {
                fn constant_buffer(&self, _slot: u32) -> Option<BufferBinding<'_>> {
                    None
                }

                fn constant_buffer_scratch(&self, _slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
                    None
                }

                fn texture2d(&self, _slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn texture2d_array(
                    &self,
                    _slot: u32,
                ) -> Option<(TextureViewId, &wgpu::TextureView)> {
                    None
                }

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_storage(&self) -> &wgpu::Buffer {
                    self.dummy_storage
                }

                fn dummy_storage_texture_view(
                    &self,
                    _format: crate::StorageTextureFormat,
                ) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d
                }

                fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view_2d_array
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider = DummyProvider {
                dummy_uniform: &dummy_uniform,
                dummy_storage: &dummy_storage,
                dummy_texture_view_2d: &dummy_texture_view_2d,
                dummy_texture_view_2d_array: &dummy_texture_view_2d_array,
                default_sampler: &default_sampler,
            };

            let mut bind_group_cache = BindGroupCache::new(16);
            let bg1 = build_bind_group(
                device,
                &mut bind_group_cache,
                &info.group_layouts[0],
                &info.group_bindings[0],
                &provider,
            )
            .expect("bind group should build");
            let stats = bind_group_cache.stats();
            assert_eq!(stats.misses, 1);
            assert_eq!(stats.hits, 0);

            let bg2 = build_bind_group(
                device,
                &mut bind_group_cache,
                &info.group_layouts[0],
                &info.group_bindings[0],
                &provider,
            )
            .expect("bind group should be cached");
            let stats = bind_group_cache.stats();
            assert_eq!(stats.misses, 1);
            assert_eq!(stats.hits, 1);
            assert!(Arc::ptr_eq(&bg1, &bg2));
        });
    }

    #[test]
    fn wgpu_exec_compute_stage_bindings_use_group_2() {
        pollster::block_on(async {
            let (adapter, device, queue) = match new_device_queue_for_tests().await {
                Ok(v) => v,
                Err(err) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

            // Some wgpu backends (notably downlevel GL/WebGL) may not support compute.
            let downlevel = adapter.get_downlevel_capabilities();
            if !downlevel
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
            {
                skip_or_panic(module_path!(), "adapter does not support compute shaders");
                return;
            }

            // Minimal SM5 compute module:
            // - numthreads(1,1,1)
            // - store_raw u0[0] = 0x12345678
            let module = crate::Sm4Module {
                stage: crate::ShaderStage::Compute,
                model: crate::ShaderModel { major: 5, minor: 0 },
                decls: vec![crate::Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
                instructions: vec![
                    crate::Sm4Inst::StoreRaw {
                        uav: crate::UavRef { slot: 0 },
                        addr: crate::SrcOperand {
                            kind: crate::SrcKind::ImmediateF32([0, 0, 0, 0]),
                            swizzle: crate::Swizzle::XYZW,
                            modifier: crate::OperandModifier::None,
                        },
                        value: crate::SrcOperand {
                            kind: crate::SrcKind::ImmediateF32([0x1234_5678, 0, 0, 0]),
                            swizzle: crate::Swizzle::XYZW,
                            modifier: crate::OperandModifier::None,
                        },
                        mask: crate::WriteMask::X,
                    },
                    crate::Sm4Inst::Ret,
                ],
            };

            // Translate to WGSL.
            let dxbc_bytes = dxbc_test_utils::build_container(&[]);
            let dxbc = crate::DxbcFile::parse(&dxbc_bytes).expect("minimal dxbc parse");
            let signatures = crate::ShaderSignatures::default();
            let translation =
                crate::translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap();

            // Validate WGSL via naga.
            let module = naga::front::wgsl::parse_str(&translation.wgsl)
                .expect("generated compute WGSL failed to parse");
            let mut validator = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            validator
                .validate(&module)
                .expect("generated compute WGSL failed to validate");

            // Build pipeline layout with empty group(0) and group(1), plus group(2) from
            // reflection.
            let mut layout_cache = BindGroupLayoutCache::new();
            let pipeline_bindings = build_pipeline_bindings_info(
                &device,
                &mut layout_cache,
                [ShaderBindingSet::Guest(
                    translation.reflection.bindings.as_slice(),
                )],
                BindGroupIndexValidation::GuestShaders,
            )
            .expect("build_pipeline_bindings_info");
            assert_eq!(
                pipeline_bindings.group_layouts.len(),
                3,
                "expected empty bind groups 0 and 1 plus compute group 2"
            );

            let u0_binding = translation
                .reflection
                .bindings
                .iter()
                .find(|b| matches!(b.kind, crate::BindingKind::UavBuffer { slot: 0 }))
                .expect("expected reflected u0 binding");
            assert_eq!(u0_binding.group, 2);
            assert!(
                translation
                    .wgsl
                    .contains(&format!("@group(2) @binding({})", u0_binding.binding)),
                "expected translated compute WGSL to declare u0 in @group(2); WGSL was:\n{}",
                translation.wgsl
            );

            let bgl_refs: Vec<&wgpu::BindGroupLayout> = pipeline_bindings
                .group_layouts
                .iter()
                .map(|l| l.layout.as_ref())
                .collect();
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("reflection_bindings compute test pipeline layout"),
                bind_group_layouts: &bgl_refs,
                push_constant_ranges: &[],
            });

            // Create storage buffer backing `u0`.
            let u0_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings compute test u0 buffer"),
                size: 4,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&u0_buffer, 0, &0u32.to_le_bytes());

            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings compute test staging buffer"),
                size: 4,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let bg0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("reflection_bindings compute test bg0"),
                layout: pipeline_bindings.group_layouts[0].layout.as_ref(),
                entries: &[],
            });
            let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("reflection_bindings compute test bg1"),
                layout: pipeline_bindings.group_layouts[1].layout.as_ref(),
                entries: &[],
            });
            let bg2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("reflection_bindings compute test bg2"),
                layout: pipeline_bindings.group_layouts[2].layout.as_ref(),
                entries: &[wgpu::BindGroupEntry {
                    binding: u0_binding.binding,
                    resource: u0_buffer.as_entire_binding(),
                }],
            });

            // Compile compute pipeline.
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            let cs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("reflection_bindings compute test shader"),
                source: wgpu::ShaderSource::Wgsl(translation.wgsl.into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("reflection_bindings compute test pipeline"),
                layout: Some(&pipeline_layout),
                module: &cs,
                entry_point: "cs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            });
            if let Some(err) = device.pop_error_scope().await {
                skip_or_panic(
                    module_path!(),
                    &format!("compute pipeline creation failed ({err:?})"),
                );
                return;
            }

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("reflection_bindings compute test encoder"),
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("reflection_bindings compute test pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bg0, &[]);
                pass.set_bind_group(1, &bg1, &[]);
                pass.set_bind_group(2, &bg2, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            encoder.copy_buffer_to_buffer(&u0_buffer, 0, &staging, 0, 4);
            queue.submit([encoder.finish()]);

            let bytes = read_buffer(&device, &staging)
                .await
                .expect("read back staging buffer");
            let value = u32::from_le_bytes(bytes[..4].try_into().unwrap());
            assert_eq!(value, 0x1234_5678);
        });
    }
}
