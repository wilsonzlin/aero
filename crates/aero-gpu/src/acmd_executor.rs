use std::collections::HashMap;

use crate::protocol::{parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError};
use crate::readback_rgba8;
use crate::texture_manager::TextureRegion;
use crate::GpuError;

#[derive(Debug, thiserror::Error)]
pub enum AeroGpuAcmdExecutorError {
    #[error("failed to parse AeroGPU command stream: {0}")]
    Parse(#[from] AeroGpuCmdStreamParseError),
    #[error("unsupported texture format {0}")]
    UnsupportedTextureFormat(u32),
    #[error("unknown texture handle {0}")]
    UnknownTexture(u32),
    #[error("no render target bound")]
    NoRenderTarget,
    #[error("wgpu backend error: {0}")]
    Backend(String),
}

#[derive(Debug)]
struct Texture2d {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// Minimal host-side executor for the versioned AeroGPU command stream (`aerogpu_cmd.h`).
///
/// This is intended as a wiring layer for the emulator so that guest submissions (ring +
/// `ACMD` command buffers) can result in actual command execution and scanout output.
///
/// The implementation is intentionally minimal and currently supports only a subset of commands
/// required for end-to-end integration tests (texture creation, render target binding, clear,
/// present, and readback).
pub struct AeroGpuAcmdExecutor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    textures: HashMap<u32, Texture2d>,
    render_target: u32,
    presented_scanouts: HashMap<u32, u32>,
}

impl AeroGpuAcmdExecutor {
    pub async fn new_headless() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // Prefer "native" backends; this avoids initializing GL stacks in headless CI.
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| GpuError::Backend("no suitable wgpu adapter found".into()))?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-gpu ACMD executor"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|err| GpuError::Backend(err.to_string()))?;

        Ok(Self {
            device,
            queue,
            textures: HashMap::new(),
            render_target: 0,
            presented_scanouts: HashMap::new(),
        })
    }

    pub fn reset(&mut self) {
        self.textures.clear();
        self.render_target = 0;
        self.presented_scanouts.clear();
    }

    pub fn execute_submission(
        &mut self,
        cmd_stream_bytes: &[u8],
        _alloc_table_bytes: Option<&[u8]>,
    ) -> Result<(), AeroGpuAcmdExecutorError> {
        if cmd_stream_bytes.is_empty() {
            return Ok(());
        }

        let stream = parse_cmd_stream(cmd_stream_bytes)?;
        for cmd in stream.cmds {
            match cmd {
                AeroGpuCmd::CreateTexture2d {
                    texture_handle,
                    format,
                    width,
                    height,
                    ..
                } => {
                    let format = map_texture_format(format)?;

                    let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("aerogpu texture2d"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::COPY_SRC
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });

                    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    self.textures.insert(
                        texture_handle,
                        Texture2d {
                            width,
                            height,
                            format,
                            texture,
                            view,
                        },
                    );
                }
                AeroGpuCmd::SetRenderTargets {
                    color_count,
                    colors,
                    ..
                } => {
                    self.render_target = if color_count == 0 { 0 } else { colors[0] };
                }
                AeroGpuCmd::Clear {
                    flags,
                    color_rgba_f32,
                    ..
                } => {
                    if (flags & 1) == 0 {
                        // Only color clear is currently supported.
                        continue;
                    }

                    let rt = self.render_target;
                    if rt == 0 {
                        return Err(AeroGpuAcmdExecutorError::NoRenderTarget);
                    }
                    let tex = self
                        .textures
                        .get(&rt)
                        .ok_or(AeroGpuAcmdExecutorError::UnknownTexture(rt))?;

                    let color = wgpu::Color {
                        r: f32::from_bits(color_rgba_f32[0]) as f64,
                        g: f32::from_bits(color_rgba_f32[1]) as f64,
                        b: f32::from_bits(color_rgba_f32[2]) as f64,
                        a: f32::from_bits(color_rgba_f32[3]) as f64,
                    };

                    let mut encoder =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("aerogpu clear encoder"),
                            });

                    {
                        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("aerogpu clear"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &tex.view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(color),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        });
                    }

                    self.queue.submit([encoder.finish()]);

                    #[cfg(not(target_arch = "wasm32"))]
                    self.device.poll(wgpu::Maintain::Wait);

                    #[cfg(target_arch = "wasm32")]
                    self.device.poll(wgpu::Maintain::Poll);
                }
                AeroGpuCmd::Present { scanout_id, .. }
                | AeroGpuCmd::PresentEx { scanout_id, .. } => {
                    // Minimal semantics: present the currently bound render target.
                    if self.render_target != 0 {
                        self.presented_scanouts
                            .insert(scanout_id, self.render_target);
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    pub async fn read_presented_scanout_rgba8(
        &self,
        scanout_id: u32,
    ) -> Result<Option<(u32, u32, Vec<u8>)>, AeroGpuAcmdExecutorError> {
        let Some(&tex_handle) = self.presented_scanouts.get(&scanout_id) else {
            return Ok(None);
        };
        let tex = self
            .textures
            .get(&tex_handle)
            .ok_or(AeroGpuAcmdExecutorError::UnknownTexture(tex_handle))?;

        let mut rgba8 = readback_rgba8(
            &self.device,
            &self.queue,
            &tex.texture,
            TextureRegion::full(wgpu::Extent3d {
                width: tex.width,
                height: tex.height,
                depth_or_array_layers: 1,
            }),
        )
        .await;

        match tex.format {
            wgpu::TextureFormat::Rgba8Unorm => {}
            wgpu::TextureFormat::Bgra8Unorm => {
                // Convert BGRA -> RGBA.
                for px in rgba8.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
            }
            _ => {
                return Err(AeroGpuAcmdExecutorError::Backend(format!(
                    "readback_rgba8 only supports RGBA/BGRA textures (got {:?})",
                    tex.format
                )))
            }
        }

        Ok(Some((tex.width, tex.height, rgba8)))
    }
}

fn map_texture_format(format: u32) -> Result<wgpu::TextureFormat, AeroGpuAcmdExecutorError> {
    Ok(match format {
        1 | 2 => wgpu::TextureFormat::Bgra8Unorm, // B8G8R8A8/B8G8R8X8
        3 | 4 => wgpu::TextureFormat::Rgba8Unorm, // R8G8B8A8/R8G8B8X8
        other => return Err(AeroGpuAcmdExecutorError::UnsupportedTextureFormat(other)),
    })
}
