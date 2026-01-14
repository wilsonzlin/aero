use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use super::{stable_hash64, CacheStats};

#[derive(Clone, Debug)]
pub struct CachedBindGroupLayout {
    pub hash: u64,
    pub layout: Arc<wgpu::BindGroupLayout>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct BindGroupLayoutEntryKey {
    binding: u32,
    visibility: u32,
    ty: BindingTypeKey,
    count: Option<u32>,
}

impl BindGroupLayoutEntryKey {
    fn from_entry(entry: &wgpu::BindGroupLayoutEntry) -> Self {
        Self {
            binding: entry.binding,
            visibility: entry.visibility.bits(),
            ty: BindingTypeKey::from_binding_type(&entry.ty),
            count: entry.count.map(NonZeroU32::get),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum BindingTypeKey {
    Buffer {
        ty: wgpu::BufferBindingType,
        has_dynamic_offset: bool,
        min_binding_size: Option<u64>,
    },
    Sampler(wgpu::SamplerBindingType),
    Texture {
        sample_type: wgpu::TextureSampleType,
        view_dimension: wgpu::TextureViewDimension,
        multisampled: bool,
    },
    StorageTexture {
        access: wgpu::StorageTextureAccess,
        format: wgpu::TextureFormat,
        view_dimension: wgpu::TextureViewDimension,
    },
    Other(String),
}

impl BindingTypeKey {
    fn from_binding_type(ty: &wgpu::BindingType) -> Self {
        match ty {
            wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset,
                min_binding_size,
            } => Self::Buffer {
                ty: *ty,
                has_dynamic_offset: *has_dynamic_offset,
                min_binding_size: min_binding_size.map(|s| s.get()),
            },
            wgpu::BindingType::Sampler(s) => Self::Sampler(*s),
            wgpu::BindingType::Texture {
                sample_type,
                view_dimension,
                multisampled,
            } => Self::Texture {
                sample_type: *sample_type,
                view_dimension: *view_dimension,
                multisampled: *multisampled,
            },
            wgpu::BindingType::StorageTexture {
                access,
                format,
                view_dimension,
            } => Self::StorageTexture {
                access: *access,
                format: *format,
                view_dimension: *view_dimension,
            },
            // New binding types should update the cache key to avoid accidental aliasing.
            _ => Self::Other(format!("{ty:?}")),
        }
    }
}

type BindGroupLayoutKey = Vec<BindGroupLayoutEntryKey>;

#[derive(Debug, Default)]
pub struct BindGroupLayoutCache {
    layouts: HashMap<BindGroupLayoutKey, CachedBindGroupLayout>,
    hits: u64,
    misses: u64,
}

impl BindGroupLayoutCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(
        &mut self,
        device: &wgpu::Device,
        entries: &[wgpu::BindGroupLayoutEntry],
    ) -> CachedBindGroupLayout {
        let mut entries_sorted: Vec<wgpu::BindGroupLayoutEntry> = entries.to_vec();
        entries_sorted.sort_by_key(|e| e.binding);

        let key: BindGroupLayoutKey = entries_sorted
            .iter()
            .map(BindGroupLayoutEntryKey::from_entry)
            .collect();

        if let Some(layout) = self.layouts.get(&key) {
            // The empty bind group layout is extremely common and often pre-seeded by runtimes
            // during initialization. Treating it as a regular cache hit makes higher-level cache
            // stats noisy (every pipeline that needs an empty placeholder group would report a hit).
            //
            // Keep `hits` focused on "real" reuse of non-empty layouts.
            if !key.is_empty() {
                self.hits += 1;
            }
            return layout.clone();
        }

        self.misses += 1;
        let hash = stable_hash64(&key);

        let layout = {
            #[cfg(debug_assertions)]
            let label = Some(format!("aero_bind_group_layout_{hash:016x}"));
            #[cfg(not(debug_assertions))]
            let label: Option<String> = None;

            Arc::new(
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: label.as_deref(),
                    entries: &entries_sorted,
                }),
            )
        };

        let cached = CachedBindGroupLayout { hash, layout };
        self.layouts.insert(key, cached.clone());
        cached
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
    use std::num::NonZeroU64;

    fn ubo_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(64),
            },
            count: None,
        }
    }

    #[test]
    fn layout_key_is_order_independent() {
        let mut a = [ubo_entry(2), ubo_entry(0), ubo_entry(1)];
        let mut b = [ubo_entry(0), ubo_entry(1), ubo_entry(2)];

        a.sort_by_key(|e| e.binding);
        b.sort_by_key(|e| e.binding);

        let a_key: Vec<_> = a.iter().map(BindGroupLayoutEntryKey::from_entry).collect();
        let b_key: Vec<_> = b.iter().map(BindGroupLayoutEntryKey::from_entry).collect();

        assert_eq!(a_key, b_key);
        assert_eq!(stable_hash64(&a_key), stable_hash64(&b_key));
    }

    #[test]
    fn layout_key_includes_min_binding_size() {
        let mut a = ubo_entry(0);
        let b = ubo_entry(0);
        if let wgpu::BindingType::Buffer {
            min_binding_size, ..
        } = &mut a.ty
        {
            *min_binding_size = NonZeroU64::new(128);
        }
        assert_ne!(
            BindGroupLayoutEntryKey::from_entry(&a),
            BindGroupLayoutEntryKey::from_entry(&b)
        );
    }
}
