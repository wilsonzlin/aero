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
    let mut groups: BTreeMap<u32, BTreeMap<u32, crate::Binding>> = BTreeMap::new();
    for shader in shader_bindings {
        for binding in shader {
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
                let mut size = None;
                let mut total_size = 0u64;

                if let Some(bound) = provider.constant_buffer(*slot) {
                    id = bound.id;
                    buffer = bound.buffer;
                    offset = bound.offset;
                    size = bound.size;
                    total_size = bound.total_size;
                }

                if id != BufferId(0) {
                    let required_min = (*reg_count as u64).saturating_mul(16);

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
}
