use anyhow::{anyhow, Result};

use super::{format_info, D3DFormat, GuestResourceId, ResourceManager, TextureUsageKind};

#[derive(Clone, Copy, Debug)]
pub struct RenderTargetDesc {
    pub width: u32,
    pub height: u32,
    pub format: D3DFormat,
}

#[derive(Debug)]
pub struct RenderTarget {
    desc: RenderTargetDesc,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
}

impl RenderTarget {
    pub fn desc(&self) -> &RenderTargetDesc {
        &self.desc
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DepthStencilDesc {
    pub width: u32,
    pub height: u32,
    pub format: D3DFormat,
}

#[derive(Debug)]
pub struct DepthStencil {
    desc: DepthStencilDesc,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
}

impl DepthStencil {
    pub fn desc(&self) -> &DepthStencilDesc {
        &self.desc
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
}

impl ResourceManager {
    pub fn create_render_target(
        &mut self,
        id: GuestResourceId,
        desc: RenderTargetDesc,
    ) -> Result<()> {
        if self.render_targets.contains_key(&id) {
            return Err(anyhow!("render target id already exists: {}", id));
        }

        let info = format_info(
            desc.format,
            self.device.features(),
            TextureUsageKind::RenderTarget,
        )?;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d9.render_target"),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: info.wgpu,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.render_targets.insert(
            id,
            RenderTarget {
                desc,
                texture,
                view,
                format: info.wgpu,
            },
        );
        Ok(())
    }

    pub fn render_target(&self, id: GuestResourceId) -> Result<&RenderTarget> {
        self.render_targets
            .get(&id)
            .ok_or_else(|| anyhow!("render target not found: {}", id))
    }

    pub fn destroy_render_target(&mut self, id: GuestResourceId) -> bool {
        self.render_targets.remove(&id).is_some()
    }

    pub fn create_depth_stencil(
        &mut self,
        id: GuestResourceId,
        desc: DepthStencilDesc,
    ) -> Result<()> {
        if self.depth_stencils.contains_key(&id) {
            return Err(anyhow!("depth stencil id already exists: {}", id));
        }

        let info = format_info(
            desc.format,
            self.device.features(),
            TextureUsageKind::DepthStencil,
        )?;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d9.depth_stencil"),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: info.wgpu,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.depth_stencils.insert(
            id,
            DepthStencil {
                desc,
                texture,
                view,
                format: info.wgpu,
            },
        );
        Ok(())
    }

    pub fn depth_stencil(&self, id: GuestResourceId) -> Result<&DepthStencil> {
        self.depth_stencils
            .get(&id)
            .ok_or_else(|| anyhow!("depth stencil not found: {}", id))
    }

    pub fn destroy_depth_stencil(&mut self, id: GuestResourceId) -> bool {
        self.depth_stencils.remove(&id).is_some()
    }
}
