use std::collections::HashMap;
use std::sync::Arc;

use aero_gpu::bindings::CacheStats;
use aero_gpu::pipeline_key::PipelineLayoutKey;

/// Small cache for `wgpu::PipelineLayout` objects.
///
/// Pipeline layouts are purely a function of their bind group layouts, so they can be cached and
/// reused across repeated pipeline creation / render-pass rebuilds.
#[derive(Debug, Default)]
pub struct PipelineLayoutCache {
    layouts: HashMap<PipelineLayoutKey, Arc<wgpu::PipelineLayout>>,
    hits: u64,
    misses: u64,
}

impl PipelineLayoutCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(
        &mut self,
        device: &wgpu::Device,
        key: &PipelineLayoutKey,
        bind_group_layouts: &[&wgpu::BindGroupLayout],
    ) -> Arc<wgpu::PipelineLayout> {
        if let Some(layout) = self.layouts.get(key) {
            self.hits += 1;
            return layout.clone();
        }

        self.misses += 1;

        let layout = Arc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero pipeline layout"),
            bind_group_layouts,
            push_constant_ranges: &[],
        }));

        self.layouts.insert(key.clone(), layout.clone());
        layout
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            entries: self.layouts.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_gpu::bindings::layout_cache::BindGroupLayoutCache;
    use std::num::NonZeroU64;

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
    fn pipeline_layout_is_cached_by_key() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await
            {
                Ok(rt) => rt,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };
            let device = rt.device();

            let mut bind_group_layout_cache = BindGroupLayoutCache::new();
            let cached_bgl = bind_group_layout_cache.get_or_create(
                device,
                &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(64),
                    },
                    count: None,
                }],
            );

            let key = PipelineLayoutKey {
                bind_group_layout_hashes: vec![cached_bgl.hash],
            };

            let mut cache = PipelineLayoutCache::new();
            let bind_group_layouts = [cached_bgl.layout.as_ref()];

            let a = cache.get_or_create(device, &key, &bind_group_layouts);
            let b = cache.get_or_create(device, &key, &bind_group_layouts);

            assert!(Arc::ptr_eq(&a, &b));
            assert_eq!(
                cache.stats(),
                CacheStats {
                    hits: 1,
                    misses: 1,
                    entries: 1
                }
            );
        });
    }
}

