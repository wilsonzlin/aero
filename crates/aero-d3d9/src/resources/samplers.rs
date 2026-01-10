use anyhow::{anyhow, Result};

use super::{GuestResourceId, ResourceManager};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterMode {
    Point,
    Linear,
    Anisotropic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AddressMode {
    Wrap,
    Mirror,
    Clamp,
    Border,
}

#[derive(Clone, Copy, Debug)]
pub struct SamplerDesc {
    pub filter: FilterMode,
    pub address_u: AddressMode,
    pub address_v: AddressMode,
    pub address_w: AddressMode,
    pub max_anisotropy: u16,
}

impl Default for SamplerDesc {
    fn default() -> Self {
        Self {
            filter: FilterMode::Linear,
            address_u: AddressMode::Wrap,
            address_v: AddressMode::Wrap,
            address_w: AddressMode::Wrap,
            max_anisotropy: 1,
        }
    }
}

#[derive(Debug)]
pub struct Sampler {
    desc: SamplerDesc,
    sampler: wgpu::Sampler,
}

impl Sampler {
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    pub fn desc(&self) -> &SamplerDesc {
        &self.desc
    }
}

fn address_mode(device: &wgpu::Device, mode: AddressMode) -> wgpu::AddressMode {
    match mode {
        AddressMode::Wrap => wgpu::AddressMode::Repeat,
        AddressMode::Mirror => wgpu::AddressMode::MirrorRepeat,
        AddressMode::Clamp => wgpu::AddressMode::ClampToEdge,
        AddressMode::Border => {
            if device
                .features()
                .contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER)
            {
                wgpu::AddressMode::ClampToBorder
            } else {
                wgpu::AddressMode::ClampToEdge
            }
        }
    }
}

impl ResourceManager {
    pub fn create_sampler(&mut self, id: GuestResourceId, desc: SamplerDesc) -> Result<()> {
        if self.samplers.contains_key(&id) {
            return Err(anyhow!("sampler id already exists: {}", id));
        }

        let (min_filter, mag_filter, mipmap_filter, anisotropy_clamp) = match desc.filter {
            FilterMode::Point => (
                wgpu::FilterMode::Nearest,
                wgpu::FilterMode::Nearest,
                wgpu::FilterMode::Nearest,
                1,
            ),
            FilterMode::Linear => (
                wgpu::FilterMode::Linear,
                wgpu::FilterMode::Linear,
                wgpu::FilterMode::Linear,
                1,
            ),
            FilterMode::Anisotropic => (
                wgpu::FilterMode::Linear,
                wgpu::FilterMode::Linear,
                wgpu::FilterMode::Linear,
                desc.max_anisotropy.max(1),
            ),
        };

        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aero-d3d9.sampler"),
            address_mode_u: address_mode(&self.device, desc.address_u),
            address_mode_v: address_mode(&self.device, desc.address_v),
            address_mode_w: address_mode(&self.device, desc.address_w),
            mag_filter,
            min_filter,
            mipmap_filter,
            lod_min_clamp: 0.0,
            lod_max_clamp: 32.0,
            compare: None,
            anisotropy_clamp,
            border_color: None,
        });

        self.samplers.insert(id, Sampler { desc, sampler });
        Ok(())
    }

    pub fn sampler(&self, id: GuestResourceId) -> Result<&Sampler> {
        self.samplers
            .get(&id)
            .ok_or_else(|| anyhow!("sampler not found: {}", id))
    }

    pub fn destroy_sampler(&mut self, id: GuestResourceId) -> bool {
        self.samplers.remove(&id).is_some()
    }
}
