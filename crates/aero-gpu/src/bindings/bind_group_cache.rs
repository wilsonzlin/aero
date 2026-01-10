use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;

use super::CacheStats;
use crate::bindings::layout_cache::CachedBindGroupLayout;
use crate::bindings::samplers::SamplerId;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct BufferId(pub u64);

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct TextureViewId(pub u64);

#[derive(Clone, Copy, Debug)]
pub enum BindGroupCacheResource<'a> {
    Buffer {
        id: BufferId,
        buffer: &'a wgpu::Buffer,
        offset: u64,
        size: Option<wgpu::BufferSize>,
    },
    TextureView {
        id: TextureViewId,
        view: &'a wgpu::TextureView,
    },
    Sampler {
        id: SamplerId,
        sampler: &'a wgpu::Sampler,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct BindGroupCacheEntry<'a> {
    pub binding: u32,
    pub resource: BindGroupCacheResource<'a>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct BindGroupKey {
    pub layout_hash: u64,
    pub entries: Vec<BindGroupEntryKey>,
}

impl BindGroupKey {
    pub fn new(layout_hash: u64, entries: &[BindGroupEntryKey]) -> Self {
        let mut entries = entries.to_vec();
        entries.sort_by_key(|e| e.binding);
        Self {
            layout_hash,
            entries,
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct BindGroupEntryKey {
    pub binding: u32,
    pub resource: BindGroupResourceKey,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum BindGroupResourceKey {
    Buffer {
        id: BufferId,
        offset: u64,
        size: Option<u64>,
    },
    TextureView {
        id: TextureViewId,
    },
    Sampler {
        id: SamplerId,
    },
}

impl BindGroupEntryKey {
    fn from_entry(entry: &BindGroupCacheEntry<'_>) -> Self {
        let resource = match entry.resource {
            BindGroupCacheResource::Buffer {
                id, offset, size, ..
            } => BindGroupResourceKey::Buffer {
                id,
                offset,
                size: size.map(|s| s.get()),
            },
            BindGroupCacheResource::TextureView { id, .. } => {
                BindGroupResourceKey::TextureView { id }
            }
            BindGroupCacheResource::Sampler { id, .. } => BindGroupResourceKey::Sampler { id },
        };
        Self {
            binding: entry.binding,
            resource,
        }
    }
}

#[derive(Debug)]
pub struct BindGroupCache<V> {
    cache: LruCache<BindGroupKey, V>,
    hits: u64,
    misses: u64,
}

impl<V> BindGroupCache<V> {
    pub fn new(capacity: usize) -> Self {
        let capacity =
            NonZeroUsize::new(capacity).expect("BindGroupCache capacity must be non-zero");
        Self {
            cache: LruCache::new(capacity),
            hits: 0,
            misses: 0,
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            entries: self.cache.len(),
        }
    }
}

impl<V: Clone> BindGroupCache<V> {
    pub fn get_or_create_with<F>(&mut self, key: BindGroupKey, create: F) -> V
    where
        F: FnOnce() -> V,
    {
        if let Some(value) = self.cache.get(&key) {
            self.hits += 1;
            return value.clone();
        }

        self.misses += 1;
        let value = create();
        self.cache.put(key, value.clone());
        value
    }
}

impl BindGroupCache<Arc<wgpu::BindGroup>> {
    pub fn get_or_create(
        &mut self,
        device: &wgpu::Device,
        layout: &CachedBindGroupLayout,
        entries: &[BindGroupCacheEntry<'_>],
    ) -> Arc<wgpu::BindGroup> {
        let mut keys: Vec<BindGroupEntryKey> =
            entries.iter().map(BindGroupEntryKey::from_entry).collect();
        keys.sort_by_key(|e| e.binding);

        let key = BindGroupKey::new(layout.hash, &keys);
        self.get_or_create_with(key, || {
            let mut wgpu_entries: Vec<wgpu::BindGroupEntry<'_>> = entries
                .iter()
                .map(|entry| {
                    let resource = match entry.resource {
                        BindGroupCacheResource::Buffer {
                            buffer,
                            offset,
                            size,
                            ..
                        } => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer,
                            offset,
                            size,
                        }),
                        BindGroupCacheResource::TextureView { view, .. } => {
                            wgpu::BindingResource::TextureView(view)
                        }
                        BindGroupCacheResource::Sampler { sampler, .. } => {
                            wgpu::BindingResource::Sampler(sampler)
                        }
                    };
                    wgpu::BindGroupEntry {
                        binding: entry.binding,
                        resource,
                    }
                })
                .collect();

            wgpu_entries.sort_by_key(|e| e.binding);

            #[cfg(debug_assertions)]
            let label = Some(format!("aero_bind_group_{:016x}", layout.hash));
            #[cfg(not(debug_assertions))]
            let label: Option<String> = None;

            Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: label.as_deref(),
                layout: layout.layout.as_ref(),
                entries: &wgpu_entries,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn key(layout_hash: u64, buffer_id: u64) -> BindGroupKey {
        BindGroupKey::new(
            layout_hash,
            &[BindGroupEntryKey {
                binding: 0,
                resource: BindGroupResourceKey::Buffer {
                    id: BufferId(buffer_id),
                    offset: 0,
                    size: Some(256),
                },
            }],
        )
    }

    #[test]
    fn repeated_key_hits_cache() {
        let mut cache = BindGroupCache::<u32>::new(8);
        let created = Cell::new(0);

        let k = key(0x11, 0x22);
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
    fn lru_eviction_respects_capacity() {
        let mut cache = BindGroupCache::<u32>::new(2);
        cache.get_or_create_with(key(1, 1), || 1);
        cache.get_or_create_with(key(2, 2), || 2);
        // Touch key(1, 1) so key(2, 2) becomes the LRU entry.
        cache.get_or_create_with(key(1, 1), || 1);
        cache.get_or_create_with(key(3, 3), || 3);
        assert_eq!(cache.cache.len(), 2);
        assert!(cache.cache.contains(&key(1, 1)));
        assert!(cache.cache.contains(&key(3, 3)));
        assert!(!cache.cache.contains(&key(2, 2)));
    }
}
