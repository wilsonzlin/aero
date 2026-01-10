use std::collections::HashMap;
use std::sync::Arc;

use super::CacheStats;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct SamplerId(u64);

impl SamplerId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug)]
pub struct CachedSampler {
    pub id: SamplerId,
    pub sampler: Arc<wgpu::Sampler>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct SamplerKey {
    address_mode_u: wgpu::AddressMode,
    address_mode_v: wgpu::AddressMode,
    address_mode_w: wgpu::AddressMode,
    mag_filter: wgpu::FilterMode,
    min_filter: wgpu::FilterMode,
    mipmap_filter: wgpu::FilterMode,
    compare: Option<wgpu::CompareFunction>,
    anisotropy_clamp: u16,
    lod_min_clamp_bits: u32,
    lod_max_clamp_bits: u32,
    border_color: Option<wgpu::SamplerBorderColor>,
}

impl SamplerKey {
    fn from_desc(desc: &wgpu::SamplerDescriptor<'_>) -> Self {
        Self {
            address_mode_u: desc.address_mode_u,
            address_mode_v: desc.address_mode_v,
            address_mode_w: desc.address_mode_w,
            mag_filter: desc.mag_filter,
            min_filter: desc.min_filter,
            mipmap_filter: desc.mipmap_filter,
            compare: desc.compare,
            anisotropy_clamp: desc.anisotropy_clamp,
            lod_min_clamp_bits: normalize_f32_for_key(desc.lod_min_clamp),
            lod_max_clamp_bits: normalize_f32_for_key(desc.lod_max_clamp),
            border_color: desc.border_color,
        }
    }
}

fn normalize_f32_for_key(value: f32) -> u32 {
    // `f32` is not `Eq` due to NaN semantics. For cache keys we want bitwise stability,
    // but also want equivalent values like `-0.0` and `0.0` to hit the cache.
    if value == 0.0 {
        0.0f32.to_bits()
    } else if value.is_nan() {
        f32::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

#[derive(Debug, Default)]
pub struct SamplerCache {
    next_id: u64,
    samplers: HashMap<SamplerKey, CachedSampler>,
    hits: u64,
    misses: u64,
}

impl SamplerCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(
        &mut self,
        device: &wgpu::Device,
        desc: &wgpu::SamplerDescriptor<'_>,
    ) -> CachedSampler {
        let key = SamplerKey::from_desc(desc);
        if let Some(sampler) = self.samplers.get(&key) {
            self.hits += 1;
            return sampler.clone();
        }

        self.misses += 1;
        let id = SamplerId(self.next_id);
        self.next_id += 1;

        let sampler = {
            #[cfg(debug_assertions)]
            let label = Some(format!(
                "aero_sampler_{}_u{:?}_v{:?}_w{:?}_mag{:?}_min{:?}_mip{:?}_cmp{:?}_aniso{}_lod{:08x}-{:08x}",
                id.get(),
                desc.address_mode_u,
                desc.address_mode_v,
                desc.address_mode_w,
                desc.mag_filter,
                desc.min_filter,
                desc.mipmap_filter,
                desc.compare,
                desc.anisotropy_clamp,
                key.lod_min_clamp_bits,
                key.lod_max_clamp_bits,
            ));
            #[cfg(not(debug_assertions))]
            let label: Option<String> = None;

            let descriptor = wgpu::SamplerDescriptor {
                label: label.as_deref(),
                address_mode_u: desc.address_mode_u,
                address_mode_v: desc.address_mode_v,
                address_mode_w: desc.address_mode_w,
                mag_filter: desc.mag_filter,
                min_filter: desc.min_filter,
                mipmap_filter: desc.mipmap_filter,
                lod_min_clamp: desc.lod_min_clamp,
                lod_max_clamp: desc.lod_max_clamp,
                compare: desc.compare,
                anisotropy_clamp: desc.anisotropy_clamp,
                border_color: desc.border_color,
            };
            Arc::new(device.create_sampler(&descriptor))
        };

        let cached = CachedSampler { id, sampler };
        self.samplers.insert(key, cached.clone());
        cached
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            entries: self.samplers.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_desc<'a>(label: Option<&'a str>) -> wgpu::SamplerDescriptor<'a> {
        wgpu::SamplerDescriptor {
            label,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            lod_min_clamp: 0.0,
            lod_max_clamp: 32.0,
            compare: None,
            anisotropy_clamp: 1,
            border_color: None,
        }
    }

    #[test]
    fn sampler_key_ignores_label() {
        let a = base_desc(Some("a"));
        let b = base_desc(Some("b"));
        assert_eq!(SamplerKey::from_desc(&a), SamplerKey::from_desc(&b));
    }

    #[test]
    fn sampler_key_normalizes_negative_zero() {
        let mut a = base_desc(None);
        let mut b = base_desc(None);
        a.lod_min_clamp = -0.0;
        b.lod_min_clamp = 0.0;
        assert_eq!(SamplerKey::from_desc(&a), SamplerKey::from_desc(&b));
    }

    #[test]
    fn sampler_key_normalizes_nan_payloads() {
        let mut a = base_desc(None);
        let mut b = base_desc(None);
        a.lod_max_clamp = f32::from_bits(0x7fc0_0001);
        b.lod_max_clamp = f32::from_bits(0x7fc0_1234);
        assert_eq!(SamplerKey::from_desc(&a), SamplerKey::from_desc(&b));
    }
}
