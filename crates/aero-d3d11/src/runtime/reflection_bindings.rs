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
    BINDING_BASE_CBUFFER, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, MAX_CBUFFER_SLOTS,
    MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS,
};

/// The AeroGPU D3D11 binding model uses stage-scoped bind groups:
/// - `@group(0)` = vertex shader resources
/// - `@group(1)` = pixel/fragment shader resources
/// - `@group(2)` = compute shader resources
const MAX_BIND_GROUP_INDEX: u32 = 2;
pub(super) const UNIFORM_BINDING_SIZE_ALIGN: u64 = 16;

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
) -> Result<PipelineBindingsInfo>
where
    I: IntoIterator<Item = &'a [crate::Binding]>,
{
    let max_uniform_binding_size = device.limits().max_uniform_buffer_binding_size as u64;

    let mut groups: BTreeMap<u32, BTreeMap<u32, crate::Binding>> = BTreeMap::new();
    for shader in shader_bindings {
        for binding in shader {
            if binding.group > MAX_BIND_GROUP_INDEX {
                bail!(
                    "binding @group({}) is out of range for AeroGPU D3D11 binding model (max {})",
                    binding.group,
                    MAX_BIND_GROUP_INDEX
                );
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
                group_map.insert(binding.binding, binding.clone());
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

    let max_group = groups
        .keys()
        .copied()
        .max()
        .expect("groups.is_empty handled above");
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

pub(super) fn binding_to_layout_entry(binding: &crate::Binding) -> Result<wgpu::BindGroupLayoutEntry> {
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
    fn texture2d(&self, slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)>;
    fn sampler(&self, slot: u32) -> Option<&CachedSampler>;

    fn dummy_uniform(&self) -> &wgpu::Buffer;
    fn dummy_texture_view(&self) -> &wgpu::TextureView;
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
                            } else if offset != 0 && offset % uniform_align != 0 {
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
            crate::BindingKind::Texture2D { slot } => {
                let (id, view) = provider
                    .texture2d(*slot)
                    .unwrap_or((TextureViewId(0), provider.dummy_texture_view()));

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
        }
    }

    Ok(cache.get_or_create(device, layout, &entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_gpu::bindings::samplers::SamplerCache;

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

    fn skip_or_panic(test_name: &str, reason: &str) {
        if require_webgpu() {
            panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
        }
        eprintln!("skipping {test_name}: {reason}");
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
                [vs.as_slice(), ps.as_slice()],
            )
            .unwrap();
            let info_b = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [ps.as_slice(), vs.as_slice()],
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
                [vs.as_slice(), ps.as_slice()],
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
                [empty.as_slice(), cs.as_slice()],
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
            assert_eq!(info.layout_key.bind_group_layout_hashes[0], info.group_layouts[0].hash);
            assert_eq!(info.layout_key.bind_group_layout_hashes[1], info.group_layouts[1].hash);
            assert_eq!(info.layout_key.bind_group_layout_hashes[2], info.group_layouts[2].hash);
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
                [a.as_slice(), b.as_slice()],
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
                group: MAX_BIND_GROUP_INDEX + 1,
                binding: BINDING_BASE_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                kind: crate::BindingKind::Texture2D { slot: 0 },
            }];

            let err = build_pipeline_bindings_info(
                device,
                &mut layout_cache,
                [bindings.as_slice()],
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
                    skip_or_panic(module_path!(), "cannot construct reg_count over device limit");
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
                [bindings.as_slice()],
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

            let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection_bindings test dummy uniform"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
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
            let dummy_texture_view =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());

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
                dummy_texture_view: &'a wgpu::TextureView,
                default_sampler: &'a CachedSampler,
            }

            impl BindGroupResourceProvider for TestProvider<'_> {
                fn constant_buffer(&self, slot: u32) -> Option<BufferBinding<'_>> {
                    if slot != 0 {
                        return None;
                    }
                    self.cb.map(|(id, buffer, offset, size, total_size)| BufferBinding {
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

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_texture_view(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider_too_small = TestProvider {
                cb: Some((BufferId(1), &too_small_uniform, 0, None, 32)),
                dummy_uniform: &dummy_uniform,
                dummy_texture_view: &dummy_texture_view,
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
                &[binding.clone()],
                &provider_too_small,
            )
            .unwrap();
            let bg_dummy = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                &[binding],
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
            if offset == 0 || uniform_align <= 1 || offset % uniform_align == 0 {
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
            let dummy_texture_view = dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());

            let mut sampler_cache = SamplerCache::new();
            let default_sampler = sampler_cache.get_or_create(device, &wgpu::SamplerDescriptor::default());

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
                dummy_texture_view: &'a wgpu::TextureView,
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

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_texture_view(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view
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
                dummy_texture_view: &dummy_texture_view,
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
                &[binding.clone()],
                &provider_with_scratch,
            )
            .unwrap();
            let bg_dummy = build_bind_group(
                device,
                &mut bind_group_cache,
                &layout,
                &[binding],
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
    }

    #[test]
    fn pipeline_bindings_merge_and_bind_group_caching() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await {
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
                [bindings_a.as_slice(), bindings_b.as_slice()],
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
            let dummy_texture_view =
                dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());

            let mut sampler_cache = SamplerCache::new();
            let default_sampler =
                sampler_cache.get_or_create(device, &wgpu::SamplerDescriptor::default());

            struct DummyProvider<'a> {
                dummy_uniform: &'a wgpu::Buffer,
                dummy_texture_view: &'a wgpu::TextureView,
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

                fn sampler(&self, _slot: u32) -> Option<&CachedSampler> {
                    None
                }

                fn dummy_uniform(&self) -> &wgpu::Buffer {
                    self.dummy_uniform
                }

                fn dummy_texture_view(&self) -> &wgpu::TextureView {
                    self.dummy_texture_view
                }

                fn default_sampler(&self) -> &CachedSampler {
                    self.default_sampler
                }
            }

            let provider = DummyProvider {
                dummy_uniform: &dummy_uniform,
                dummy_texture_view: &dummy_texture_view,
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
}
