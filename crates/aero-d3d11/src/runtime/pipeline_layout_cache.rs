use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;

use aero_gpu::bindings::CacheStats;
use aero_gpu::pipeline_key::PipelineLayoutKey;

/// Cache for values keyed by [`PipelineLayoutKey`].
///
/// This is primarily used to cache `wgpu::PipelineLayout` objects: multiple render
/// pipelines can share identical bind-group layouts (e.g. differing vertex buffer
/// layouts or blend state), and creating a fresh `wgpu::PipelineLayout` on every
/// pipeline miss is avoidable overhead.
#[derive(Debug)]
pub struct PipelineLayoutCache<V> {
    layouts: HashMap<PipelineLayoutKey, V>,
    hits: u64,
    misses: u64,
}

impl<V> Default for PipelineLayoutCache<V> {
    fn default() -> Self {
        Self {
            layouts: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }
}

impl<V> PipelineLayoutCache<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            entries: self.layouts.len(),
        }
    }

    pub fn clear(&mut self) {
        self.layouts.clear();
        self.hits = 0;
        self.misses = 0;
    }
}

impl<V: Clone> PipelineLayoutCache<V> {
    pub fn get_or_create_with<F>(&mut self, key: PipelineLayoutKey, create: F) -> V
    where
        F: FnOnce() -> V,
    {
        match self.layouts.entry(key) {
            Entry::Occupied(entry) => {
                self.hits += 1;
                entry.get().clone()
            }
            Entry::Vacant(entry) => {
                self.misses += 1;
                let value = create();
                entry.insert(value.clone());
                value
            }
        }
    }
}

impl PipelineLayoutCache<Arc<wgpu::PipelineLayout>> {
    pub fn get_or_create(
        &mut self,
        device: &wgpu::Device,
        key: PipelineLayoutKey,
        bind_group_layouts: &[&wgpu::BindGroupLayout],
        label: Option<&str>,
    ) -> Arc<wgpu::PipelineLayout> {
        self.get_or_create_with(key, || {
            Arc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label,
                bind_group_layouts,
                push_constant_ranges: &[],
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_gpu::bindings::layout_cache::BindGroupLayoutCache;
    use std::cell::Cell;
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

    fn key(groups: &[u64]) -> PipelineLayoutKey {
        PipelineLayoutKey {
            bind_group_layout_hashes: groups.to_vec(),
        }
    }

    #[test]
    fn repeated_key_hits_cache() {
        let mut cache = PipelineLayoutCache::<u32>::new();
        let created = Cell::new(0);

        let k = key(&[0x11, 0x22]);
        for _ in 0..4 {
            let v = cache.get_or_create_with(k.clone(), || {
                let next = created.get() + 1;
                created.set(next);
                next
            });
            assert_eq!(v, 1);
        }

        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 3);
        assert_eq!(stats.entries, 1);
    }

    #[test]
    fn different_keys_miss_independently() {
        let mut cache = PipelineLayoutCache::<u32>::new();

        let v1 = cache.get_or_create_with(key(&[1]), || 10);
        let v2 = cache.get_or_create_with(key(&[2]), || 20);
        let v1_again = cache.get_or_create_with(key(&[1]), || 30);

        assert_eq!(v1, 10);
        assert_eq!(v2, 20);
        assert_eq!(v1_again, 10);

        let stats = cache.stats();
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.entries, 2);
    }

    #[test]
    fn pipeline_layout_is_cached_by_key() {
        pollster::block_on(async {
            let rt = match crate::runtime::aerogpu_execute::AerogpuCmdRuntime::new_for_tests().await {
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

            let mut cache: PipelineLayoutCache<Arc<wgpu::PipelineLayout>> = PipelineLayoutCache::new();
            let bind_group_layouts = [cached_bgl.layout.as_ref()];

            let a = cache.get_or_create(
                device,
                key.clone(),
                &bind_group_layouts,
                Some("aero pipeline layout"),
            );
            let b = cache.get_or_create(
                device,
                key.clone(),
                &bind_group_layouts,
                Some("aero pipeline layout"),
            );

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

